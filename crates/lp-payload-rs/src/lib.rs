//! `lp-payload-rs` — Android OTA `payload.bin` container reader/extractor.
//!
//! Parses the `CrAU` file header and the protobuf [`DeltaArchiveManifest`],
//! then rebuilds partition images by replaying operations (`REPLACE`,
//! `REPLACE_BZ`, `REPLACE_XZ`, raw type `14` `REPLACE_ZSTD`, `ZERO`,
//! `DISCARD`) onto their `dst_extents`, with SHA256 verification at operation
//! (compressed blob) and partition (final image) level. Proven end-to-end on
//! a real 8.2 GB ColorOS 16 payload: all 46 partitions decoded byte-identical
//! to independently dumped images (`cargo test -- --ignored`,
//! `cph2487_full_dump_cross_hash`).
//!
//! Delta/source operations (`SOURCE_COPY`, `SOURCE_BSDIFF`, `PUFFDIFF`,
//! `ZUCCHINI`, `BROTLI_BSDIFF`, `LZ4DIFF_*`) are recognized but rejected with
//! [`PayloadError::UnsupportedOperation`] — they need the old-slot images and
//! patch codecs (planned next; `puffdiff`/`bsdiff-android`/`lz4diff` crates
//! already cover the patch side).
//!
//! Container layout, verified byte-for-byte against a real ColorOS 16
//! (CPH2487) payload and `platform/system/update_engine/README.md` §"Update
//! Payload File Specification" (vendored under `third_party/aosp/`):
//!
//! ```text
//! offset  size  field
//!   0      4    magic "CrAU"
//!   4      8    major version   (u64 BE)
//!  12      8    manifest size   (u64 BE)
//!  20      4    metadata-signature size (u32 BE, only when version >= 2)
//!  24      n    DeltaArchiveManifest (protobuf)
//!  24+n    b   Signatures blob (b = metadata-signature size)
//!  24+n+b ...  payload data blobs; operation `data_offset`s are relative to
//!              the first byte AFTER the signatures blob
//! ```
//!
//! The blob position is empirically pinned on the CPH2487 payload: at 24+n it
//! starts as protobuf `0a 88 02 …` (field 1, length 264 ⇒ 267 B total, exactly
//! the header's u32), and the very next byte is the XZ magic `fd 37 7a 58 5a`
//! of the first operation's data. Payloads following the Chrome OS writer
//! convention (blob inlined between header and manifest) will fail manifest
//! decoding here rather than silently misparse.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use prost::Message;
use sha2::{Digest, Sha256};

pub mod proto {
    #![allow(clippy::doc_overindented_list_items)] // verbatim comments from update_metadata.proto
    include!("chromeos_update_engine.rs");
}

pub use proto::{
    DeltaArchiveManifest, DynamicPartitionGroup, DynamicPartitionMetadata, Extent,
    InstallOperation, PartitionInfo, PartitionUpdate, Signatures, install_operation,
};

/// Operation type enum (`REPLACE`, `REPLACE_BZ`, ...).
pub type OperationType = install_operation::Type;

/// Payload file magic.
pub const PAYLOAD_MAGIC: [u8; 4] = *b"CrAU";

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("bad magic: expected 'CrAU', found {0:?} (not a payload.bin?)")]
    BadMagic([u8; 4]),
    #[error("unsupported payload major version {0} (reader supports 1-3)")]
    UnsupportedVersion(u64),
    #[error("payload truncated while reading {context} (wanted {wanted} more bytes)")]
    Truncated {
        context: &'static str,
        wanted: usize,
    },
    #[error("manifest protobuf decode failed at byte offset {manifest_offset}: {source}")]
    ManifestDecode {
        manifest_offset: u64,
        #[source]
        source: prost::DecodeError,
    },
    #[error("install operation is missing required field '{0}'")]
    MalformedOperation(&'static str),
    #[error("operation type {0} not supported by this reader (raw/bz/xz/zstd/zero/discard only)")]
    UnsupportedOperation(String),
    #[error("partition '{0}' not found in payload manifest")]
    NoSuchPartition(String),
    #[error("data_sha256 mismatch for operation #{op_index} of partition '{partition}'")]
    HashMismatch { partition: String, op_index: usize },
    #[error("partition hash mismatch for '{0}' (manifest new_partition_info.hash)")]
    PartitionHashMismatch(String),
    #[error(
        "operation #{op_index} of partition '{partition}' produced {got} bytes, manifest wants {want}"
    )]
    LengthMismatch {
        partition: String,
        op_index: usize,
        got: u64,
        want: u64,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, PayloadError>;

/// Parsed fixed-size `CrAU` header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub major_version: u64,
    pub manifest_size: u64,
    /// Size of the metadata-signature blob located between manifest and data.
    pub metadata_signature_size: u64,
    /// Byte size of the fixed header (20 for v1, 24 for v2+).
    pub header_size: u64,
}

impl Header {
    pub fn parse<R: Read>(reader: &mut R) -> Result<Self> {
        let mut magic = [0u8; 4];
        read_exact_checked(reader, &mut magic, "magic")?;
        if magic != PAYLOAD_MAGIC {
            return Err(PayloadError::BadMagic(magic));
        }
        let major_version = read_u64_be(reader)?;
        if !(1..=3).contains(&major_version) {
            return Err(PayloadError::UnsupportedVersion(major_version));
        }
        let manifest_size = read_u64_be(reader)?;
        let metadata_signature_size = if major_version >= 2 {
            // Android payloads store this as u32 (README table).
            let mut b = [0u8; 4];
            read_exact_checked(reader, &mut b, "metadata signature size")?;
            u32::from_be_bytes(b) as u64
        } else {
            0
        };
        let header_size = 4 + 8 + 8 + if major_version >= 2 { 4 } else { 0 };
        Ok(Self {
            major_version,
            manifest_size,
            metadata_signature_size,
            header_size,
        })
    }
}

/// Decode a stored `r#type` code into the AOSP enum when known.
pub fn op_type(op: &InstallOperation) -> Option<OperationType> {
    OperationType::try_from(op.r#type).ok()
}

/// Human-readable operation type name (also for codes outside the vendored
/// proto, e.g. `REPLACE_ZSTD(14)` used by VABC payloads).
pub fn op_type_name(op: &InstallOperation) -> String {
    match op.r#type {
        14 => "REPLACE_ZSTD".to_string(),
        n => match OperationType::try_from(n) {
            Ok(t) => t.as_str_name().to_string(),
            Err(_) => format!("UNKNOWN({n})"),
        },
    }
}

/// Extractable operation kinds this reader understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Raw,
    Bz,
    Xz,
    Zstd,
    Zero,
    Discard,
}

pub fn op_kind(op: &InstallOperation) -> Option<OpKind> {
    match op.r#type {
        0 => Some(OpKind::Raw),
        1 => Some(OpKind::Bz),
        8 => Some(OpKind::Xz),
        14 => Some(OpKind::Zstd),
        6 => Some(OpKind::Zero),
        7 => Some(OpKind::Discard),
        _ => None,
    }
}

/// `dst_extents` as absolute byte ranges.
fn extent_bytes(extents: &[Extent], block_size: u64) -> Vec<(u64, u64)> {
    extents
        .iter()
        .map(|e| {
            (
                e.start_block.unwrap_or(0) * block_size,
                e.num_blocks.unwrap_or(0) * block_size,
            )
        })
        .filter(|(_, len)| *len > 0)
        .collect()
}

/// `Read` adapter that taps every byte passing through into a shared SHA256
/// and counts bytes consumed from the inner reader.
struct Tap<R> {
    inner: R,
    sink: Rc<RefCell<Sha256>>,
    consumed: Rc<std::cell::Cell<u64>>,
}

impl<R: Read> Read for Tap<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.sink.borrow_mut().update(&buf[..n]);
            self.consumed.set(self.consumed.get() + n as u64);
        }
        Ok(n)
    }
}

/// `Write` sink that only feeds a SHA256 (used to drain blob padding tails).
struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Position-target writer: distributes one sequential stream across a queue
/// of `(offset, length)` extents, seeking to each extent start.
struct Scatter<'a, W: Write + Seek> {
    out: &'a mut W,
    queue: VecDeque<(u64, u64)>,
    written: u64,
}

impl<'a, W: Write + Seek> Scatter<'a, W> {
    fn new(out: &'a mut W, mut extents: Vec<(u64, u64)>) -> Self {
        extents.sort_by_key(|(o, _)| *o);
        Self {
            out,
            queue: extents.into(),
            written: 0,
        }
    }
}

impl<W: Write + Seek> Write for Scatter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.queue.front() {
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "scatter extents exhausted",
                    ));
                }
                Some((_, 0)) => {
                    self.queue.pop_front();
                }
                Some((off, remain)) => {
                    self.out.seek(SeekFrom::Start(*off))?;
                    let n = buf.len().min(*remain as usize);
                    self.out.write_all(&buf[..n])?;
                    let entry = self.queue.front_mut().unwrap();
                    entry.0 += n as u64;
                    entry.1 -= n as u64;
                    self.written += n as u64;
                    if entry.1 == 0 {
                        self.queue.pop_front();
                    }
                    return Ok(n);
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// An opened payload.bin: header, decoded manifest and a live reader for the
/// data blob region.
#[derive(Debug)]
pub struct Payload<R: Read + Seek> {
    reader: R,
    header: Header,
    manifest: DeltaArchiveManifest,
    data_start: u64,
}

impl Payload<File> {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let file = File::open(path)?;
        Self::from_reader(file)
    }
}

impl<R: Read + Seek> Payload<R> {
    /// Read and decode header + manifest.
    pub fn from_reader(mut reader: R) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let header = Header::parse(&mut reader)?;
        let manifest_offset = reader.stream_position()?;
        let mut buf = vec![0u8; header.manifest_size as usize];
        read_exact_checked(&mut reader, &mut buf, "manifest")?;
        let manifest = DeltaArchiveManifest::decode(buf.as_slice()).map_err(|source| {
            PayloadError::ManifestDecode {
                manifest_offset,
                source,
            }
        })?;
        // Data region starts after the metadata-signature blob, which sits
        // physically between the manifest and the first op blob (verified
        // empirically; see module docs).
        let data_start = manifest_offset + header.manifest_size + header.metadata_signature_size;
        Ok(Self {
            reader,
            header,
            manifest,
            data_start,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn manifest(&self) -> &DeltaArchiveManifest {
        &self.manifest
    }

    /// Absolute byte offset where operation data blobs begin.
    pub fn data_start(&self) -> u64 {
        self.data_start
    }

    pub fn block_size(&self) -> u64 {
        self.manifest.block_size.unwrap_or(4096) as u64
    }

    pub fn partitions(&self) -> &[PartitionUpdate] {
        &self.manifest.partitions
    }

    pub fn partition(&self, name: &str) -> Result<&PartitionUpdate> {
        self.partitions()
            .iter()
            .find(|p| p.partition_name == name)
            .ok_or_else(|| PayloadError::NoSuchPartition(name.to_string()))
    }

    /// Absolute (offset, length) of an operation's data blob inside the file.
    pub fn op_data_range(&self, op: &InstallOperation) -> Result<(u64, u64)> {
        let off = self.data_start
            + op.data_offset
                .ok_or(PayloadError::MalformedOperation("data_offset"))?;
        let len = op
            .data_length
            .ok_or(PayloadError::MalformedOperation("data_length"))?;
        Ok((off, len))
    }

    /// Sum of all operation data lengths (the payload blob region size).
    pub fn total_data_size(&self) -> u64 {
        self.partitions()
            .iter()
            .flat_map(|p| p.operations.iter())
            .map(|op| op.data_length.unwrap_or(0))
            .sum()
    }

    /// Stream-verify one operation's stored blob against its `data_sha256_hash`
    /// (checks the RAW compressed bytes at `op_data_range`; no decoding).
    /// Returns `Ok(true)` on match, `Ok(false)` when the op carries no hash.
    pub fn check_op_data(&mut self, op: &InstallOperation) -> Result<bool> {
        let Some(expected) = op.data_sha256_hash.as_ref() else {
            return Ok(false);
        };
        let (off, len) = self.op_data_range(op)?;
        self.reader.seek(SeekFrom::Start(off))?;
        let mut h = Sha256::new();
        let mut buf = [0u8; 128 * 1024];
        let mut remaining = len;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = self.reader.read(&mut buf[..want])?;
            if n == 0 {
                return Err(PayloadError::Truncated {
                    context: "operation data",
                    wanted: remaining as usize,
                });
            }
            h.update(&buf[..n]);
            remaining -= n as u64;
        }
        Ok(h.finalize()[..] == expected[..])
    }

    /// Convenience: rebuild a partition into `path` (freshly created, zero
    /// extended to the manifest size). Verifies op hashes always, partition
    /// hash too when present. Returns bytes placed.
    pub fn extract_partition_to_file(&mut self, name: &str, path: &std::path::Path) -> Result<u64> {
        let size = self
            .partition(name)?
            .new_partition_info
            .as_ref()
            .and_then(|i| i.size)
            .unwrap_or(0);
        #[cfg(feature = "trace-steps")]
        eprintln!("[trace] {name}: create {}", path.display());
        let file = File::create(path)?;
        file.set_len(size)?;
        let mut w = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
        #[cfg(feature = "trace-steps")]
        eprintln!("[trace] {name}: extracting {size} B");
        let placed = self.extract_partition(name, &mut w, true)?;
        #[cfg(feature = "trace-steps")]
        eprintln!("[trace] {name}: ops done placed={placed}, flushing");
        let file = w.into_inner().map_err(|e| e.into_error())?;
        drop(file);
        #[cfg(feature = "trace-steps")]
        eprintln!("[trace] {name}: verifying image hash");
        // File::create handles are write-only; reopen read-only for the hash
        // readback.
        let mut rf = File::open(path)?;
        self.verify_partition_hash(&mut rf, name)?;
        Ok(placed)
    }

    /// Re-read `[0, new_partition_info.size)` from a previously written
    /// output and compare against `new_partition_info.hash`. No-op when the
    /// manifest carries no image hash.
    pub fn verify_partition_hash<W: Read + Seek>(&mut self, out: &mut W, name: &str) -> Result<()> {
        let info = self.partition(name)?.new_partition_info.clone();
        let Some(info) = info else { return Ok(()) };
        let Some(expected) = info.hash else {
            return Ok(());
        };
        let size = info.size.unwrap_or(0);
        out.seek(SeekFrom::Start(0))?;
        let mut h = Sha256::new();
        let mut buf = [0u8; 256 * 1024];
        let mut remaining = size;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = out.read(&mut buf[..want])?;
            if n == 0 {
                return Err(PayloadError::Truncated {
                    context: "partition readback",
                    wanted: remaining as usize,
                });
            }
            h.update(&buf[..n]);
            remaining -= n as u64;
        }
        if h.finalize()[..] != expected[..] {
            return Err(PayloadError::PartitionHashMismatch(name.to_string()));
        }
        Ok(())
    }

    /// Replay all operations of a partition onto `out`, positioning each op's
    /// decoded stream across its `dst_extents`.
    ///
    /// `out` should be pre-sized to `new_partition_info.size` (zero-filled);
    /// `DISCARD`/`ZERO` holes rely on that. When `verify`, each op's SHA256 is
    /// checked against `data_sha256_hash` (compressed bytes) and, at the end,
    /// `out` is read back over the partition size and compared against
    /// `new_partition_info.hash`.
    ///
    /// Aborts with [`PayloadError::UnsupportedOperation`] during a pre-scan
    /// (before writing) if any op needs a delta/source codec.
    pub fn extract_partition<W>(&mut self, name: &str, out: &mut W, verify: bool) -> Result<u64>
    where
        W: Write + Seek,
    {
        let partition = self.partition(name)?;
        for op in partition.operations.iter() {
            if op_kind(op).is_none() {
                return Err(PayloadError::UnsupportedOperation(op_type_name(op)));
            }
        }
        // Clone small metadata so the manifest borrow ends before &mut self.
        let ops: Vec<InstallOperation> = partition.operations.clone();
        let bs = self.block_size();

        let mut placed = 0u64;
        for (i, op) in ops.iter().enumerate() {
            let kind = op_kind(op).expect("pre-scanned");
            let dst = extent_bytes(&op.dst_extents, bs);
            let dst_total: u64 = dst.iter().map(|(_, l)| *l).sum();
            let expect_len = op.dst_length.unwrap_or(dst_total);

            match kind {
                OpKind::Discard => {
                    // hole: leave pre-zeroed space untouched
                }
                OpKind::Zero => {
                    let mut scatter = Scatter::new(out, dst);
                    write_zeros(&mut scatter, expect_len)?;
                    placed += expect_len;
                }
                _ => {
                    // NOTE: AOSP `data_sha256_hash` covers the FULL data blob
                    // INCLUDING alignment padding. Decoders stop at their own
                    // stream end, so after decoding we drain whatever the
                    // decoder left unconsumed within [off, off+len) through
                    // the same hasher before comparing.
                    let (off, len) = self.op_data_range(op)?;
                    self.reader.seek(SeekFrom::Start(off))?;
                    let sink = Rc::new(RefCell::new(Sha256::new()));
                    let consumed = Rc::new(std::cell::Cell::new(0u64));
                    let tap = Tap {
                        inner: (&mut self.reader).take(len),
                        sink: Rc::clone(&sink),
                        consumed: Rc::clone(&consumed),
                    };
                    let decoded = {
                        let mut dec: Box<dyn Read> = match kind {
                            OpKind::Raw => Box::new(tap) as Box<dyn Read>,
                            OpKind::Bz => Box::new(bzip2::read::BzDecoder::new(tap)),
                            OpKind::Xz => Box::new(xz2::read::XzDecoder::new(tap)),
                            OpKind::Zstd => Box::new(
                                ruzstd::decoding::StreamingDecoder::new(tap)
                                    .map_err(io::Error::other)?,
                            ),
                            _ => unreachable!("filtered in pre-scan"),
                        };
                        // Decode limited to expect_len onto the scatter.
                        let mut scatter = Scatter::new(out, dst);
                        let mut limited = (&mut dec as &mut dyn Read).take(expect_len);
                        let got = io::copy(&mut limited, &mut scatter)?;
                        if got != expect_len {
                            return Err(PayloadError::LengthMismatch {
                                partition: name.to_string(),
                                op_index: i,
                                got,
                                want: expect_len,
                            });
                        }
                        drop(dec);
                        got
                    };
                    placed += decoded;
                    if verify && let Some(expected) = op.data_sha256_hash.as_ref() {
                        let mut h = std::mem::replace(&mut *sink.borrow_mut(), Sha256::new());
                        let used = consumed.get();
                        if used < len {
                            let mut tail = (&mut self.reader).take(len - used);
                            io::copy(&mut tail, &mut HashWriter(&mut h))?;
                        }
                        if h.finalize()[..] != expected[..] {
                            return Err(PayloadError::HashMismatch {
                                partition: name.to_string(),
                                op_index: i,
                            });
                        }
                    }
                }
            }
        }

        Ok(placed)
    }
}

fn write_zeros<W: Write + ?Sized>(w: &mut W, mut len: u64) -> io::Result<()> {
    const Z: &[u8] = &[0u8; 64 * 1024];
    while len > 0 {
        let n = len.min(Z.len() as u64) as usize;
        w.write_all(&Z[..n])?;
        len -= n as u64;
    }
    Ok(())
}

fn read_u64_be<R: Read>(reader: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    read_exact_checked(reader, &mut b, "u64 field")?;
    Ok(u64::from_be_bytes(b))
}

fn read_exact_checked<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    context: &'static str,
) -> Result<()> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(PayloadError::Truncated {
            context,
            wanted: buf.len(),
        }),
        Err(e) => Err(PayloadError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn extent(start_block: u64, num_blocks: u64) -> Extent {
        Extent {
            start_block: Some(start_block),
            num_blocks: Some(num_blocks),
        }
    }

    fn op(
        ty: i32,
        data_offset: u64,
        data_length: u64,
        dst: Vec<Extent>,
        hash: Option<Vec<u8>>,
    ) -> InstallOperation {
        InstallOperation {
            r#type: ty,
            data_offset: Some(data_offset),
            data_length: Some(data_length),
            dst_extents: dst,
            dst_length: None,
            data_sha256_hash: hash,
            ..Default::default()
        }
    }

    fn partition(
        name: &str,
        size: u64,
        ops: Vec<InstallOperation>,
        hash: Option<Vec<u8>>,
    ) -> PartitionUpdate {
        PartitionUpdate {
            partition_name: name.to_string(),
            operations: ops,
            new_partition_info: Some(PartitionInfo {
                size: Some(size),
                hash,
            }),
            ..Default::default()
        }
    }

    /// Build a synthetic Android-layout payload: header, manifest, signature
    /// blob, data (the real-world order verified on CPH2487).
    fn build_payload(
        version: u64,
        manifest: &DeltaArchiveManifest,
        data: &[u8],
        blob: &[u8],
    ) -> Vec<u8> {
        let mut mbuf = Vec::new();
        manifest.encode(&mut mbuf).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(b"CrAU");
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(mbuf.len() as u64).to_be_bytes());
        if version >= 2 {
            out.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(&mbuf);
        if version >= 2 {
            out.extend_from_slice(blob);
        }
        out.extend_from_slice(data);
        out
    }

    // block_size=16 keeps fixtures tiny: 1 block = 16 bytes.
    const BS: u32 = 16;

    #[test]
    fn synthetic_parse_and_extract() {
        let data = b"0123456789abcdef"; // 16 bytes = one block
        let hash: Vec<u8> = Sha256::digest(data).to_vec();
        // image = data block + zero block; partition hash covers both
        let mut img = Vec::new();
        img.extend_from_slice(data);
        img.resize(32, 0);
        let img_hash: Vec<u8> = Sha256::digest(&img).to_vec();

        let t = OperationType::Replace as i32;
        let z = OperationType::Zero as i32;
        let manifest = DeltaArchiveManifest {
            block_size: Some(BS),
            partitions: vec![
                partition(
                    "boot_a",
                    32,
                    vec![
                        op(t, 0, 16, vec![extent(0, 1)], Some(hash.clone())),
                        op(z, 0, 0, vec![extent(1, 1)], None),
                    ],
                    Some(img_hash),
                ),
                // Puffdiff (9) must be rejected pre-scan
                partition(
                    "delta_a",
                    16,
                    vec![op(9, 0, 4, vec![extent(0, 1)], None)],
                    None,
                ),
            ],
            ..Default::default()
        };
        let bytes = build_payload(2, &manifest, data, b"signatureblob");
        let mut payload = Payload::from_reader(Cursor::new(bytes)).unwrap();

        let h = payload.header();
        assert_eq!(h.major_version, 2);
        assert_eq!(h.header_size, 24);
        assert_eq!(h.metadata_signature_size, 13);
        assert_eq!(
            payload.data_start(),
            24 + manifest.encoded_len() as u64 + 13
        );
        // 16 (replace) + 0 (zero op contributes nothing) + 4 (replace)
        assert_eq!(payload.total_data_size(), 16 + 4);

        let mut out = Cursor::new(Vec::new());
        let placed = payload.extract_partition("boot_a", &mut out, true).unwrap();
        assert_eq!(placed, 32);
        assert_eq!(out.get_ref(), &img);
        // image-level hash is checked via the dedicated readback path
        payload.verify_partition_hash(&mut out, "boot_a").unwrap();

        let mut out = Cursor::new(Vec::new());
        let err = payload
            .extract_partition("delta_a", &mut out, false)
            .unwrap_err();
        assert!(matches!(err, PayloadError::UnsupportedOperation(_)));
        assert_eq!(out.get_ref().len(), 0);

        assert!(matches!(
            payload.partition("nope"),
            Err(PayloadError::NoSuchPartition(_))
        ));
    }

    #[test]
    fn hash_mismatch_detected() {
        let data = b"0123456789abcdef";
        let manifest = DeltaArchiveManifest {
            block_size: Some(BS),
            partitions: vec![partition(
                "boot_a",
                16,
                vec![op(
                    OperationType::Replace as i32,
                    0,
                    16,
                    vec![extent(0, 1)],
                    Some(vec![0u8; 32]),
                )],
                None,
            )],
            ..Default::default()
        };
        let bytes = build_payload(2, &manifest, data, b"");
        let mut payload = Payload::from_reader(Cursor::new(bytes)).unwrap();
        let mut out = Cursor::new(Vec::new());
        let err = payload
            .extract_partition("boot_a", &mut out, true)
            .unwrap_err();
        assert!(matches!(
            err,
            PayloadError::HashMismatch { op_index: 0, .. }
        ));
    }

    #[test]
    fn bad_magic_rejected() {
        let err = Payload::from_reader(Cursor::new(b"XXXX1234567890abcdef".to_vec())).unwrap_err();
        assert!(matches!(err, PayloadError::BadMagic(m) if m == *b"XXXX"));
    }

    #[test]
    fn unsupported_version_rejected() {
        let mut b = Vec::new();
        b.extend_from_slice(b"CrAU");
        b.extend_from_slice(&9u64.to_be_bytes());
        b.extend_from_slice(&0u64.to_be_bytes());
        let err = Payload::from_reader(Cursor::new(b)).unwrap_err();
        assert!(matches!(err, PayloadError::UnsupportedVersion(9)));
    }

    #[test]
    fn discard_leaves_hole_and_offsets_respect_extents() {
        // two raw ops placed at blocks 2 and 0 (out of order in manifest)
        let a = b"aaaaaaaaaaaaaaaa"; // 16
        let b = b"bbbbbbbbbbbbbbbb";
        let manifest = DeltaArchiveManifest {
            block_size: Some(BS),
            partitions: vec![partition(
                "mix_a",
                48,
                vec![
                    op(0, 16, 16, vec![extent(2, 1)], None), // b -> block 2
                    op(0, 0, 16, vec![extent(0, 1)], None),  // a -> block 0
                    op(7, 0, 0, vec![extent(1, 1)], None),   // DISCARD hole stays zero
                ],
                None,
            )],
            ..Default::default()
        };
        let mut data = Vec::new();
        data.extend_from_slice(a);
        data.extend_from_slice(b);
        let bytes = build_payload(2, &manifest, &data, b"");
        let mut payload = Payload::from_reader(Cursor::new(bytes)).unwrap();
        let mut out = Cursor::new(vec![0u8; 48]);
        let placed = payload.extract_partition("mix_a", &mut out, false).unwrap();
        assert_eq!(placed, 32);
        assert_eq!(&out.get_ref()[..16], a);
        assert_eq!(&out.get_ref()[16..32], &[0u8; 16]);
        assert_eq!(&out.get_ref()[32..48], b);
    }

    #[test]
    fn v1_header_has_no_signature_field() {
        let manifest = DeltaArchiveManifest {
            block_size: Some(BS),
            partitions: vec![partition(
                "rootfs",
                16,
                vec![op(0, 0, 16, vec![extent(0, 1)], None)],
                None,
            )],
            ..Default::default()
        };
        let bytes = build_payload(1, &manifest, b"0123456789abcdef", b"");
        let payload = Payload::from_reader(Cursor::new(bytes)).unwrap();
        assert_eq!(payload.header().header_size, 20);
        assert_eq!(payload.data_start(), 20 + manifest.encoded_len() as u64);
    }
}
