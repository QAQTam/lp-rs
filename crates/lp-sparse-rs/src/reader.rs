use std::io::{self, Read, Seek, SeekFrom, Write};

use crc32fast::Hasher;
use zerocopy::{FromBytes, IntoBytes};

use crate::error::{Result, SparseError};
use crate::format::{
    CHUNK_HEADER_SIZE, CHUNK_TYPE_CRC32, CHUNK_TYPE_DONT_CARE, CHUNK_TYPE_FILL, CHUNK_TYPE_RAW,
    ChunkHeaderRaw, SPARSE_HEADER_MAGIC, SPARSE_HEADER_MAJOR_VERSION, SPARSE_HEADER_SIZE,
    SparseHeaderRaw,
};

/// Decoded metadata for a single chunk inside a sparse image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SparseChunk {
    /// Raw data stored directly in the sparse file at `data_offset`.
    Raw {
        output_offset: u64,
        byte_len: u64,
        data_offset: u64,
    },
    /// Repeated 4-byte pattern.
    Fill {
        output_offset: u64,
        byte_len: u64,
        fill_bytes: [u8; 4],
    },
    /// Unspecified ("don't care") region, reads back as zeroes.
    DontCare { output_offset: u64, byte_len: u64 },
}

impl SparseChunk {
    pub fn output_offset(&self) -> u64 {
        match self {
            SparseChunk::Raw { output_offset, .. }
            | SparseChunk::Fill { output_offset, .. }
            | SparseChunk::DontCare { output_offset, .. } => *output_offset,
        }
    }

    pub fn byte_len(&self) -> u64 {
        match self {
            SparseChunk::Raw { byte_len, .. }
            | SparseChunk::Fill { byte_len, .. }
            | SparseChunk::DontCare { byte_len, .. } => *byte_len,
        }
    }
}

/// Checks if the stream starts with the Android sparse header magic (`0xed26ff3a`),
/// restoring the stream position afterward.
pub fn is_sparse_image<R: Read + Seek>(reader: &mut R) -> Result<bool> {
    let start = reader.stream_position()?;
    let mut magic_bytes = [0u8; 4];
    let res = match reader.read_exact(&mut magic_bytes) {
        Ok(()) => Ok(u32::from_le_bytes(magic_bytes) == SPARSE_HEADER_MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(SparseError::Io(e)),
    };
    reader.seek(SeekFrom::Start(start))?;
    res
}

/// Seekable, streaming reader that exposes an Android sparse image as a virtual raw byte stream.
#[derive(Debug)]
pub struct SparseReader<R> {
    inner: R,
    block_size: u32,
    total_blocks: u64,
    total_size: u64,
    header_checksum: u32,
    chunks: Vec<SparseChunk>,
    pos: u64,
}

impl<R: Read + Seek> SparseReader<R> {
    /// Parses the sparse header and chunk table from `reader` (starting at offset 0).
    pub fn open(mut reader: R) -> Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let mut hdr_buf = [0u8; SPARSE_HEADER_SIZE];
        reader.read_exact(&mut hdr_buf)?;
        let hdr = SparseHeaderRaw::read_from_bytes(&hdr_buf)
            .map_err(|_| SparseError::InvalidHeader("failed to parse sparse header".into()))?;

        let magic = hdr.magic.get();
        if magic != SPARSE_HEADER_MAGIC {
            let hint = match magic {
                0x0000_0000 => "starts with all-zero bytes (blank or uninitialized image)",
                0x616c_4467 => {
                    "looks like a raw LpMetadataGeometry header (super_empty.img), not a sparse file"
                }
                0x414C_5030 => "looks like a raw LpMetadataHeader",
                _ => "input appears to be a raw image or corrupted file",
            };
            return Err(SparseError::InvalidMagic {
                found: magic,
                expected: SPARSE_HEADER_MAGIC,
                hint,
            });
        }

        let major = hdr.major_version.get();
        if major > SPARSE_HEADER_MAJOR_VERSION {
            return Err(SparseError::UnsupportedVersion {
                major,
                max_supported: SPARSE_HEADER_MAJOR_VERSION,
            });
        }

        let file_hdr_sz = hdr.file_hdr_sz.get() as usize;
        if file_hdr_sz < SPARSE_HEADER_SIZE {
            return Err(SparseError::InvalidHeader(format!(
                "file_hdr_sz ({file_hdr_sz}) < {SPARSE_HEADER_SIZE}"
            )));
        }
        let chunk_hdr_sz = hdr.chunk_hdr_sz.get() as usize;
        if chunk_hdr_sz < CHUNK_HEADER_SIZE {
            return Err(SparseError::InvalidHeader(format!(
                "chunk_hdr_sz ({chunk_hdr_sz}) < {CHUNK_HEADER_SIZE}"
            )));
        }

        let block_size = hdr.blk_sz.get();
        if block_size == 0 || block_size % 4 != 0 {
            return Err(SparseError::InvalidHeader(format!(
                "invalid block size {block_size}"
            )));
        }

        let total_blocks = hdr.total_blks.get() as u64;
        let total_chunks = hdr.total_chunks.get();
        let header_checksum = hdr.image_checksum.get();

        // Skip any extra file header bytes.
        if file_hdr_sz > SPARSE_HEADER_SIZE {
            reader.seek(SeekFrom::Current((file_hdr_sz - SPARSE_HEADER_SIZE) as i64))?;
        }

        let mut chunks = Vec::with_capacity(total_chunks as usize);
        let mut out_block: u64 = 0;
        let mut crc_chunk_value: Option<u32> = None;

        for idx in 0..total_chunks {
            let chunk_file_offset = reader.stream_position()?;
            let mut ch_buf = [0u8; CHUNK_HEADER_SIZE];
            reader.read_exact(&mut ch_buf)?;
            let ch = ChunkHeaderRaw::read_from_bytes(&ch_buf).map_err(|_| {
                SparseError::InvalidChunk {
                    index: idx,
                    offset: chunk_file_offset,
                    reason: "failed to parse chunk header".into(),
                }
            })?;

            if chunk_hdr_sz > CHUNK_HEADER_SIZE {
                reader.seek(SeekFrom::Current((chunk_hdr_sz - CHUNK_HEADER_SIZE) as i64))?;
            }

            let chunk_type = ch.chunk_type.get();
            let chunk_sz = ch.chunk_sz.get() as u64;
            let total_sz = ch.total_sz.get() as u64;
            let payload_sz = total_sz.checked_sub(chunk_hdr_sz as u64).ok_or_else(|| {
                SparseError::InvalidChunk {
                    index: idx,
                    offset: chunk_file_offset,
                    reason: format!("total_sz ({total_sz}) < chunk_hdr_sz ({chunk_hdr_sz})"),
                }
            })?;

            let output_offset = out_block.checked_mul(block_size as u64).ok_or_else(|| {
                SparseError::InvalidChunk {
                    index: idx,
                    offset: chunk_file_offset,
                    reason: "output offset overflow".into(),
                }
            })?;

            match chunk_type {
                CHUNK_TYPE_RAW => {
                    let expected_payload =
                        chunk_sz.checked_mul(block_size as u64).ok_or_else(|| {
                            SparseError::InvalidChunk {
                                index: idx,
                                offset: chunk_file_offset,
                                reason: "chunk byte length overflow".into(),
                            }
                        })?;
                    if payload_sz != expected_payload {
                        return Err(SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: format!(
                                "RAW payload size mismatch: expected {expected_payload} bytes ({chunk_sz} blocks * {block_size}), got {payload_sz} bytes"
                            ),
                        });
                    }
                    let data_offset = reader.stream_position()?;
                    reader.seek(SeekFrom::Current(payload_sz as i64))?;
                    if expected_payload > 0 {
                        chunks.push(SparseChunk::Raw {
                            output_offset,
                            byte_len: expected_payload,
                            data_offset,
                        });
                    }
                    out_block += chunk_sz;
                }
                CHUNK_TYPE_FILL => {
                    if payload_sz != 4 {
                        return Err(SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: format!("FILL payload must be 4 bytes, got {payload_sz}"),
                        });
                    }
                    let mut fill_bytes = [0u8; 4];
                    reader.read_exact(&mut fill_bytes)?;
                    let byte_len = chunk_sz.checked_mul(block_size as u64).ok_or_else(|| {
                        SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: "chunk byte length overflow".into(),
                        }
                    })?;
                    if byte_len > 0 {
                        chunks.push(SparseChunk::Fill {
                            output_offset,
                            byte_len,
                            fill_bytes,
                        });
                    }
                    out_block += chunk_sz;
                }
                CHUNK_TYPE_DONT_CARE => {
                    if payload_sz != 0 {
                        return Err(SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: format!("DONT_CARE payload must be 0 bytes, got {payload_sz}"),
                        });
                    }
                    let byte_len = chunk_sz.checked_mul(block_size as u64).ok_or_else(|| {
                        SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: "chunk byte length overflow".into(),
                        }
                    })?;
                    if byte_len > 0 {
                        chunks.push(SparseChunk::DontCare {
                            output_offset,
                            byte_len,
                        });
                    }
                    out_block += chunk_sz;
                }
                CHUNK_TYPE_CRC32 => {
                    if chunk_sz != 0 || payload_sz != 4 {
                        return Err(SparseError::InvalidChunk {
                            index: idx,
                            offset: chunk_file_offset,
                            reason: format!(
                                "CRC32 chunk must have chunk_sz=0 and payload=4, got chunk_sz={chunk_sz}, payload={payload_sz}"
                            ),
                        });
                    }
                    let mut crc_bytes = [0u8; 4];
                    reader.read_exact(&mut crc_bytes)?;
                    crc_chunk_value = Some(u32::from_le_bytes(crc_bytes));
                }
                other => {
                    return Err(SparseError::InvalidChunk {
                        index: idx,
                        offset: chunk_file_offset,
                        reason: format!("unknown chunk type 0x{other:04x}"),
                    });
                }
            }
        }

        if out_block != total_blocks {
            return Err(SparseError::InvalidHeader(format!(
                "total output blocks ({out_block}) != header total_blks ({total_blocks})"
            )));
        }

        let total_size = total_blocks
            .checked_mul(block_size as u64)
            .ok_or_else(|| SparseError::InvalidHeader("total image size overflow".into()))?;

        let mut sparse = Self {
            inner: reader,
            block_size,
            total_blocks,
            total_size,
            header_checksum,
            chunks,
            pos: 0,
        };

        if let Some(expected_crc) = crc_chunk_value {
            let actual = sparse.compute_crc32()?;
            if actual != expected_crc {
                return Err(SparseError::CrcMismatch {
                    expected: expected_crc,
                    actual,
                });
            }
            sparse.pos = 0;
        }

        Ok(sparse)
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub fn header_checksum(&self) -> u32 {
        self.header_checksum
    }

    pub fn chunks(&self) -> &[SparseChunk] {
        &self.chunks
    }

    /// Computes the CRC32 of the expanded image (counting `DontCare` as zeroes).
    pub fn compute_crc32(&mut self) -> Result<u32> {
        let mut hasher = Hasher::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let zero_block = vec![0u8; 1024 * 1024];

        for chunk in &self.chunks {
            match *chunk {
                SparseChunk::Raw {
                    byte_len,
                    data_offset,
                    ..
                } => {
                    self.inner.seek(SeekFrom::Start(data_offset))?;
                    let mut rem = byte_len;
                    while rem > 0 {
                        let to_read = (rem as usize).min(buf.len());
                        self.inner.read_exact(&mut buf[..to_read])?;
                        hasher.update(&buf[..to_read]);
                        rem -= to_read as u64;
                    }
                }
                SparseChunk::Fill {
                    byte_len,
                    fill_bytes,
                    ..
                } => {
                    buf.as_chunks_mut::<4>().0.fill(fill_bytes);
                    let mut rem = byte_len;
                    while rem > 0 {
                        let n = (rem as usize).min(buf.len());
                        hasher.update(&buf[..n]);
                        rem -= n as u64;
                    }
                }
                SparseChunk::DontCare { byte_len, .. } => {
                    let mut rem = byte_len;
                    while rem > 0 {
                        let n = (rem as usize).min(zero_block.len());
                        hasher.update(&zero_block[..n]);
                        rem -= n as u64;
                    }
                }
            }
        }
        Ok(hasher.finalize())
    }

    /// Expands this sparse image into a raw image (`simg2img`).
    /// Seeks over `DontCare` regions when writing to sparse-capable filesystems,
    /// and sets the final length by writing a trailing zero byte or seeking.
    pub fn expand_to<W: Write + Seek>(&mut self, writer: &mut W) -> Result<()> {
        self.expand_to_with_progress(writer, |_| {})
    }

    /// Expands this sparse image into a raw image, invoking `on_bytes(delta)` as progress advances.
    pub fn expand_to_with_progress<W: Write + Seek, F: FnMut(u64)>(
        &mut self,
        writer: &mut W,
        mut on_bytes: F,
    ) -> Result<()> {
        let mut buf = vec![0u8; 1024 * 1024];
        let mut last_was_hole = false;

        for chunk in &self.chunks {
            match *chunk {
                SparseChunk::Raw {
                    byte_len,
                    data_offset,
                    ..
                } => {
                    self.inner.seek(SeekFrom::Start(data_offset))?;
                    let mut rem = byte_len;
                    while rem > 0 {
                        let n = (rem as usize).min(buf.len());
                        self.inner.read_exact(&mut buf[..n])?;
                        writer.write_all(&buf[..n])?;
                        on_bytes(n as u64);
                        rem -= n as u64;
                    }
                    last_was_hole = false;
                }
                SparseChunk::Fill {
                    byte_len,
                    fill_bytes,
                    ..
                } => {
                    buf.as_chunks_mut::<4>().0.fill(fill_bytes);
                    let mut rem = byte_len;
                    while rem > 0 {
                        let n = (rem as usize).min(buf.len());
                        writer.write_all(&buf[..n])?;
                        on_bytes(n as u64);
                        rem -= n as u64;
                    }
                    last_was_hole = false;
                }
                SparseChunk::DontCare { byte_len, .. } => {
                    writer.seek(SeekFrom::Current(byte_len as i64))?;
                    on_bytes(byte_len);
                    last_was_hole = true;
                }
            }
        }

        if last_was_hole && self.total_size > 0 {
            writer.seek(SeekFrom::Start(self.total_size - 1))?;
            writer.write_all(&[0u8])?;
        }
        writer.flush()?;
        Ok(())
    }

    fn find_chunk_index(&self, pos: u64) -> Option<usize> {
        let idx = self
            .chunks
            .partition_point(|c| c.output_offset() <= pos)
            .checked_sub(1)?;
        let c = &self.chunks[idx];
        if pos < c.output_offset() + c.byte_len() {
            Some(idx)
        } else {
            None
        }
    }
}

impl<R: Read + Seek> Read for SparseReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.total_size {
            return Ok(0);
        }

        let idx = match self.find_chunk_index(self.pos) {
            Some(i) => i,
            None => return Ok(0),
        };

        let chunk = &self.chunks[idx];
        let offset_in_chunk = self.pos - chunk.output_offset();
        let avail = (chunk.byte_len() - offset_in_chunk) as usize;
        let to_read = out.len().min(avail);

        match *chunk {
            SparseChunk::Raw { data_offset, .. } => {
                self.inner
                    .seek(SeekFrom::Start(data_offset + offset_in_chunk))?;
                let n = self.inner.read(&mut out[..to_read])?;
                self.pos += n as u64;
                Ok(n)
            }
            SparseChunk::Fill { fill_bytes, .. } => {
                let start_mod = (offset_in_chunk % 4) as usize;
                for (i, byte) in out[..to_read].iter_mut().enumerate() {
                    *byte = fill_bytes[(start_mod + i) % 4];
                }
                self.pos += to_read as u64;
                Ok(to_read)
            }
            SparseChunk::DontCare { .. } => {
                out[..to_read].fill(0);
                self.pos += to_read as u64;
                Ok(to_read)
            }
        }
    }
}

impl<R: Read + Seek> Seek for SparseReader<R> {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let new_pos: i64 = match style {
            SeekFrom::Start(n) => i64::try_from(n)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::End(delta) => (self.total_size as i64)
                .checked_add(delta)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
            SeekFrom::Current(delta) => (self.pos as i64)
                .checked_add(delta)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek to negative position",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

/// Helper to read a `SparseHeaderRaw` without loading chunks.
pub fn read_sparse_header<R: Read + Seek>(reader: &mut R) -> Result<SparseHeaderRaw> {
    let mut hdr_buf = [0u8; SPARSE_HEADER_SIZE];
    reader.read_exact(&mut hdr_buf)?;
    let hdr = SparseHeaderRaw::read_from_bytes(&hdr_buf)
        .map_err(|_| SparseError::InvalidHeader("failed to parse sparse header".into()))?;
    let _ = hdr.as_bytes();
    Ok(hdr)
}
