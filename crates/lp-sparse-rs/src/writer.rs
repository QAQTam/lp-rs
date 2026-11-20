use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crc32fast::Hasher;
use zerocopy::IntoBytes;

use crate::error::{Result, SparseError};
use crate::format::{
    CHUNK_HEADER_SIZE, CHUNK_TYPE_CRC32, CHUNK_TYPE_DONT_CARE, CHUNK_TYPE_FILL, CHUNK_TYPE_RAW,
    ChunkHeaderRaw, SparseHeaderRaw,
};

const MAX_RAW_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
enum BackingSource {
    Memory(Vec<u8>),
    FileSlice {
        path: PathBuf,
        file_offset: u64,
        byte_len: u64,
    },
    Fill(u32),
}

#[derive(Debug, Clone)]
struct Region {
    start_block: u64,
    num_blocks: u64,
    source: BackingSource,
}

/// Builder for creating Android sparse or raw images without loading huge files into RAM.
pub struct SparseBuilder {
    block_size: u32,
    total_blocks: u64,
    total_size: u64,
    regions: BTreeMap<u64, Region>,
}

impl SparseBuilder {
    /// Creates a new `SparseBuilder` for an image of `total_size` bytes with `block_size` blocks.
    pub fn new(block_size: u32, total_size: u64) -> Result<Self> {
        if block_size == 0 || !block_size.is_multiple_of(4) {
            return Err(SparseError::InvalidArgument(format!(
                "block_size ({block_size}) must be a non-zero multiple of 4"
            )));
        }
        let total_blocks = total_size.div_ceil(block_size as u64);
        if total_blocks > u32::MAX as u64 {
            return Err(SparseError::InvalidArgument(format!(
                "total_blocks ({total_blocks}) exceeds u32::MAX"
            )));
        }
        Ok(Self {
            block_size,
            total_blocks,
            total_size,
            regions: BTreeMap::new(),
        })
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

    fn check_bounds_and_overlap(&self, start_block: u64, num_blocks: u64) -> Result<()> {
        if num_blocks == 0 {
            return Ok(());
        }
        let end_block = start_block
            .checked_add(num_blocks)
            .ok_or(SparseError::OutOfBounds {
                start_block,
                end_block: u64::MAX,
                total_blocks: self.total_blocks,
                block_size: self.block_size,
            })?;
        if end_block > self.total_blocks {
            return Err(SparseError::OutOfBounds {
                start_block,
                end_block,
                total_blocks: self.total_blocks,
                block_size: self.block_size,
            });
        }
        if let Some((_, prev)) = self.regions.range(..=start_block).next_back() {
            let prev_end = prev.start_block + prev.num_blocks;
            if prev_end > start_block {
                return Err(SparseError::OverlappingRegion {
                    start_block,
                    end_block,
                    existing_start: prev.start_block,
                    existing_end: prev_end,
                });
            }
        }
        if let Some((&next_start, next)) = self.regions.range(start_block..).next()
            && next_start < end_block
        {
            return Err(SparseError::OverlappingRegion {
                start_block,
                end_block,
                existing_start: next.start_block,
                existing_end: next.start_block + next.num_blocks,
            });
        }
        Ok(())
    }

    /// Adds an in-memory data region starting at `start_block`, zero-padding any partial tail block.
    pub fn add_data(&mut self, start_block: u64, mut data: Vec<u8>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let num_blocks = (data.len() as u64).div_ceil(self.block_size as u64);
        self.check_bounds_and_overlap(start_block, num_blocks)?;
        let padded_len = (num_blocks as usize) * (self.block_size as usize);
        data.resize(padded_len, 0);
        self.regions.insert(
            start_block,
            Region {
                start_block,
                num_blocks,
                source: BackingSource::Memory(data),
            },
        );
        Ok(())
    }

    /// Adds a 32-bit `fill_val` region of `byte_len` bytes starting at `start_block`.
    pub fn add_fill(&mut self, start_block: u64, fill_val: u32, byte_len: u64) -> Result<()> {
        if byte_len == 0 {
            return Ok(());
        }
        let num_blocks = byte_len.div_ceil(self.block_size as u64);
        self.check_bounds_and_overlap(start_block, num_blocks)?;
        self.regions.insert(
            start_block,
            Region {
                start_block,
                num_blocks,
                source: BackingSource::Fill(fill_val),
            },
        );
        Ok(())
    }

    /// Associates a slice of a file on disk starting at `start_block`.
    pub fn add_file_slice(
        &mut self,
        start_block: u64,
        path: PathBuf,
        file_offset: u64,
        byte_len: u64,
    ) -> Result<()> {
        if byte_len == 0 {
            return Ok(());
        }
        let num_blocks = byte_len.div_ceil(self.block_size as u64);
        self.check_bounds_and_overlap(start_block, num_blocks)?;
        self.regions.insert(
            start_block,
            Region {
                start_block,
                num_blocks,
                source: BackingSource::FileSlice {
                    path,
                    file_offset,
                    byte_len,
                },
            },
        );
        Ok(())
    }

    /// Writes the image in Android sparse format (`sparsify = true`).
    pub fn write_sparse<W: Write>(&self, writer: &mut W, include_crc: bool) -> Result<()> {
        self.write_sparse_with_progress(writer, include_crc, |_| {})
    }

    /// Writes the image in Android sparse format, reporting output bytes via `on_bytes`.
    pub fn write_sparse_with_progress<W: Write, F: FnMut(u64)>(
        &self,
        writer: &mut W,
        include_crc: bool,
        mut on_bytes: F,
    ) -> Result<()> {
        let max_blocks_per_raw_chunk = (MAX_RAW_CHUNK_BYTES / self.block_size as u64).max(1);

        // Count chunks up front so we can stream directly without buffering the whole image.
        let mut total_chunks: u64 = 0;
        let mut cursor_block: u64 = 0;
        for region in self.regions.values() {
            if region.start_block > cursor_block {
                total_chunks += 1; // DontCare gap
            }
            match &region.source {
                BackingSource::Memory(_) | BackingSource::FileSlice { .. } => {
                    total_chunks += region.num_blocks.div_ceil(max_blocks_per_raw_chunk);
                }
                BackingSource::Fill(_) => {
                    total_chunks += 1;
                }
            }
            cursor_block = region.start_block + region.num_blocks;
        }
        if self.total_blocks > cursor_block {
            total_chunks += 1;
        }
        if include_crc {
            total_chunks += 1;
        }

        let total_chunks_u32 = u32::try_from(total_chunks).map_err(|_| {
            SparseError::InvalidArgument(format!("chunk count ({total_chunks}) exceeds u32::MAX"))
        })?;

        let header = SparseHeaderRaw::new(
            self.block_size,
            self.total_blocks as u32,
            total_chunks_u32,
            0,
        );
        writer.write_all(header.as_bytes())?;

        let mut hasher = include_crc.then(Hasher::new);
        let zero_block = vec![0u8; (self.block_size as usize).min(1024 * 1024)];
        let mut io_buf = vec![0u8; 1024 * 1024];

        let mut cursor_block: u64 = 0;
        for region in self.regions.values() {
            if region.start_block > cursor_block {
                let gap_blocks = region.start_block - cursor_block;
                emit_dont_care(writer, gap_blocks as u32)?;
                if let Some(h) = &mut hasher {
                    hash_zeros(h, gap_blocks * self.block_size as u64, &zero_block);
                }
                on_bytes(gap_blocks * self.block_size as u64);
            }

            match &region.source {
                BackingSource::Memory(data) => {
                    let mut rem_blocks = region.num_blocks;
                    let mut offset = 0usize;
                    while rem_blocks > 0 {
                        let chunk_blocks = rem_blocks.min(max_blocks_per_raw_chunk);
                        let byte_len = (chunk_blocks as usize) * (self.block_size as usize);
                        let total_sz = (CHUNK_HEADER_SIZE as u64 + byte_len as u64) as u32;
                        let ch = ChunkHeaderRaw::new(CHUNK_TYPE_RAW, chunk_blocks as u32, total_sz);
                        writer.write_all(ch.as_bytes())?;
                        writer.write_all(&data[offset..offset + byte_len])?;
                        if let Some(h) = &mut hasher {
                            h.update(&data[offset..offset + byte_len]);
                        }
                        on_bytes(byte_len as u64);
                        offset += byte_len;
                        rem_blocks -= chunk_blocks;
                    }
                }
                BackingSource::FileSlice {
                    path,
                    file_offset,
                    byte_len,
                } => {
                    let mut file = File::open(path)?;
                    file.seek(SeekFrom::Start(*file_offset))?;
                    let mut rem_file = *byte_len;
                    let mut rem_blocks = region.num_blocks;
                    while rem_blocks > 0 {
                        let chunk_blocks = rem_blocks.min(max_blocks_per_raw_chunk);
                        let chunk_bytes = (chunk_blocks * self.block_size as u64) as u32;
                        let total_sz = (CHUNK_HEADER_SIZE as u32) + chunk_bytes;
                        let ch = ChunkHeaderRaw::new(CHUNK_TYPE_RAW, chunk_blocks as u32, total_sz);
                        writer.write_all(ch.as_bytes())?;

                        let mut to_emit = chunk_bytes as u64;
                        while to_emit > 0 {
                            let n = (to_emit as usize).min(io_buf.len());
                            let file_n = (rem_file as usize).min(n);
                            if file_n > 0 {
                                file.read_exact(&mut io_buf[..file_n])?;
                                rem_file -= file_n as u64;
                            }
                            if file_n < n {
                                io_buf[file_n..n].fill(0);
                            }
                            writer.write_all(&io_buf[..n])?;
                            if let Some(h) = &mut hasher {
                                h.update(&io_buf[..n]);
                            }
                            on_bytes(n as u64);
                            to_emit -= n as u64;
                        }
                        rem_blocks -= chunk_blocks;
                    }
                }
                BackingSource::Fill(val) => {
                    let total_sz = (CHUNK_HEADER_SIZE + 4) as u32;
                    let ch =
                        ChunkHeaderRaw::new(CHUNK_TYPE_FILL, region.num_blocks as u32, total_sz);
                    writer.write_all(ch.as_bytes())?;
                    let val_bytes = val.to_le_bytes();
                    writer.write_all(&val_bytes)?;
                    let byte_len = region.num_blocks * self.block_size as u64;
                    if let Some(h) = &mut hasher {
                        io_buf.as_chunks_mut::<4>().0.fill(val_bytes);
                        let mut rem = byte_len;
                        while rem > 0 {
                            let n = (rem as usize).min(io_buf.len());
                            h.update(&io_buf[..n]);
                            rem -= n as u64;
                        }
                    }
                    on_bytes(byte_len);
                }
            }

            cursor_block = region.start_block + region.num_blocks;
        }

        if self.total_blocks > cursor_block {
            let tail_blocks = self.total_blocks - cursor_block;
            emit_dont_care(writer, tail_blocks as u32)?;
            if let Some(h) = &mut hasher {
                hash_zeros(h, tail_blocks * self.block_size as u64, &zero_block);
            }
            on_bytes(tail_blocks * self.block_size as u64);
        }

        if let Some(h) = hasher {
            let crc = h.finalize();
            let ch = ChunkHeaderRaw::new(CHUNK_TYPE_CRC32, 0, (CHUNK_HEADER_SIZE + 4) as u32);
            writer.write_all(ch.as_bytes())?;
            writer.write_all(&crc.to_le_bytes())?;
        }

        writer.flush()?;
        Ok(())
    }

    /// Writes the image as an unsparsed raw image (`sparsify = false`).
    pub fn write_raw<W: Write + Seek>(&self, writer: &mut W) -> Result<()> {
        self.write_raw_with_progress(writer, |_| {})
    }

    /// Writes the image as an unsparsed raw image, reporting progress bytes via `on_bytes`.
    pub fn write_raw_with_progress<W: Write + Seek, F: FnMut(u64)>(
        &self,
        writer: &mut W,
        mut on_bytes: F,
    ) -> Result<()> {
        let mut buf = vec![0u8; 1024 * 1024];
        let mut cursor_block: u64 = 0;

        for region in self.regions.values() {
            if region.start_block > cursor_block {
                let skip_bytes = (region.start_block - cursor_block) * self.block_size as u64;
                writer.seek(SeekFrom::Current(skip_bytes as i64))?;
                on_bytes(skip_bytes);
            }

            match &region.source {
                BackingSource::Memory(data) => {
                    writer.write_all(data)?;
                    on_bytes(data.len() as u64);
                }
                BackingSource::FileSlice {
                    path,
                    file_offset,
                    byte_len,
                } => {
                    let mut file = File::open(path)?;
                    file.seek(SeekFrom::Start(*file_offset))?;
                    let mut rem_file = *byte_len;
                    let mut rem_out = region.num_blocks * self.block_size as u64;
                    while rem_out > 0 {
                        let n = (rem_out as usize).min(buf.len());
                        let file_n = (rem_file as usize).min(n);
                        if file_n > 0 {
                            file.read_exact(&mut buf[..file_n])?;
                            rem_file -= file_n as u64;
                        }
                        if file_n < n {
                            buf[file_n..n].fill(0);
                        }
                        writer.write_all(&buf[..n])?;
                        on_bytes(n as u64);
                        rem_out -= n as u64;
                    }
                }
                BackingSource::Fill(val) => {
                    let byte_len = region.num_blocks * self.block_size as u64;
                    if *val == 0 {
                        writer.seek(SeekFrom::Current(byte_len as i64))?;
                        on_bytes(byte_len);
                    } else {
                        let val_bytes = val.to_le_bytes();
                        buf.as_chunks_mut::<4>().0.fill(val_bytes);
                        let mut rem = byte_len;
                        while rem > 0 {
                            let n = (rem as usize).min(buf.len());
                            writer.write_all(&buf[..n])?;
                            on_bytes(n as u64);
                            rem -= n as u64;
                        }
                    }
                }
            }

            cursor_block = region.start_block + region.num_blocks;
        }

        if self.total_size > 0 {
            writer.seek(SeekFrom::Start(self.total_size - 1))?;
            // Preserve the last byte if a region actually covered the very end.
            let covered_end = self.regions.values().next_back().is_some_and(|r| {
                (r.start_block + r.num_blocks) * self.block_size as u64 >= self.total_size
                    && !matches!(r.source, BackingSource::Fill(0))
            });
            if !covered_end {
                writer.write_all(&[0u8])?;
            }
        }
        writer.flush()?;
        Ok(())
    }
}

fn emit_dont_care<W: Write>(writer: &mut W, num_blocks: u32) -> io::Result<()> {
    let ch = ChunkHeaderRaw::new(CHUNK_TYPE_DONT_CARE, num_blocks, CHUNK_HEADER_SIZE as u32);
    writer.write_all(ch.as_bytes())
}

fn hash_zeros(hasher: &mut Hasher, mut bytes: u64, zero_buf: &[u8]) {
    while bytes > 0 {
        let n = (bytes as usize).min(zero_buf.len());
        hasher.update(&zero_buf[..n]);
        bytes -= n as u64;
    }
}

/// Converts a raw input stream into an Android sparse image (`img2simg`),
/// collapsing uniform 32-bit blocks (such as all-zero blocks) into `FILL` chunks.
pub fn encode_raw_to_sparse<R: Read + Seek, W: Write>(
    reader: &mut R,
    writer: &mut W,
    block_size: u32,
    include_crc: bool,
) -> Result<()> {
    if block_size == 0 || !block_size.is_multiple_of(4) {
        return Err(SparseError::InvalidArgument(format!(
            "block_size ({block_size}) must be a non-zero multiple of 4"
        )));
    }
    let file_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let total_blocks = file_len.div_ceil(block_size as u64);
    if total_blocks > u32::MAX as u64 {
        return Err(SparseError::InvalidArgument(format!(
            "file too large ({total_blocks} blocks > u32::MAX)"
        )));
    }

    // First pass: build chunk descriptors (FileOffset / Fill) so we can stream cleanly.
    #[derive(Clone, Copy)]
    enum Pending {
        Raw { file_offset: u64, blocks: u64 },
        Fill { val: u32, blocks: u64 },
    }

    let mut ops: Vec<Pending> = Vec::new();
    let mut block_buf = vec![0u8; block_size as usize];
    let mut offset = 0u64;

    while offset < file_len {
        let to_read = ((file_len - offset) as usize).min(block_size as usize);
        reader.read_exact(&mut block_buf[..to_read])?;
        if to_read < block_size as usize {
            block_buf[to_read..].fill(0);
        }

        if let Some(fill_val) = uniform_fill_u32(&block_buf) {
            if let Some(Pending::Fill { val, blocks }) = ops.last_mut()
                && *val == fill_val
            {
                *blocks += 1;
            } else {
                ops.push(Pending::Fill {
                    val: fill_val,
                    blocks: 1,
                });
            }
        } else if let Some(Pending::Raw { blocks, .. }) = ops.last_mut()
            && *blocks * (block_size as u64) < MAX_RAW_CHUNK_BYTES
        {
            *blocks += 1;
        } else {
            ops.push(Pending::Raw {
                file_offset: offset,
                blocks: 1,
            });
        }

        offset += to_read as u64;
    }

    let total_chunks = ops.len() + usize::from(include_crc);
    let total_chunks_u32 = u32::try_from(total_chunks).map_err(|_| {
        SparseError::InvalidArgument(format!("chunk count ({total_chunks}) exceeds u32::MAX"))
    })?;

    let header = SparseHeaderRaw::new(block_size, total_blocks as u32, total_chunks_u32, 0);
    writer.write_all(header.as_bytes())?;

    let mut hasher = include_crc.then(Hasher::new);
    let mut io_buf = vec![0u8; 1024 * 1024];

    for op in ops {
        match op {
            Pending::Raw {
                file_offset,
                blocks,
            } => {
                let total_bytes = blocks * block_size as u64;
                let total_sz = (CHUNK_HEADER_SIZE as u64 + total_bytes) as u32;
                let ch = ChunkHeaderRaw::new(CHUNK_TYPE_RAW, blocks as u32, total_sz);
                writer.write_all(ch.as_bytes())?;
                reader.seek(SeekFrom::Start(file_offset))?;

                let mut rem_file = (file_len - file_offset).min(total_bytes);
                let mut rem_out = total_bytes;
                while rem_out > 0 {
                    let n = (rem_out as usize).min(io_buf.len());
                    let file_n = (rem_file as usize).min(n);
                    if file_n > 0 {
                        reader.read_exact(&mut io_buf[..file_n])?;
                        rem_file -= file_n as u64;
                    }
                    if file_n < n {
                        io_buf[file_n..n].fill(0);
                    }
                    writer.write_all(&io_buf[..n])?;
                    if let Some(h) = &mut hasher {
                        h.update(&io_buf[..n]);
                    }
                    rem_out -= n as u64;
                }
            }
            Pending::Fill { val, blocks } => {
                let ch = ChunkHeaderRaw::new(
                    CHUNK_TYPE_FILL,
                    blocks as u32,
                    (CHUNK_HEADER_SIZE + 4) as u32,
                );
                writer.write_all(ch.as_bytes())?;
                let val_bytes = val.to_le_bytes();
                writer.write_all(&val_bytes)?;
                if let Some(h) = &mut hasher {
                    io_buf.as_chunks_mut::<4>().0.fill(val_bytes);
                    let mut rem = blocks * block_size as u64;
                    while rem > 0 {
                        let n = (rem as usize).min(io_buf.len());
                        h.update(&io_buf[..n]);
                        rem -= n as u64;
                    }
                }
            }
        }
    }

    if let Some(h) = hasher {
        let crc = h.finalize();
        let ch = ChunkHeaderRaw::new(CHUNK_TYPE_CRC32, 0, (CHUNK_HEADER_SIZE + 4) as u32);
        writer.write_all(ch.as_bytes())?;
        writer.write_all(&crc.to_le_bytes())?;
    }

    writer.flush()?;
    Ok(())
}

/// Returns `Some(val)` if every 4-byte word in `block` equals `val`.
pub fn uniform_fill_u32(block: &[u8]) -> Option<u32> {
    if !block.len().is_multiple_of(4) {
        return None;
    }
    let (&first, rest) = block.as_chunks::<4>().0.split_first()?;
    rest.iter()
        .all(|&chunk| chunk == first)
        .then(|| u32::from_le_bytes(first))
}
