use zerocopy::byteorder::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const SPARSE_HEADER_MAGIC: u32 = 0xed26_ff3a;
pub const SPARSE_HEADER_MAJOR_VERSION: u16 = 1;
pub const SPARSE_HEADER_MINOR_VERSION: u16 = 0;

pub const CHUNK_TYPE_RAW: u16 = 0xCAC1;
pub const CHUNK_TYPE_FILL: u16 = 0xCAC2;
pub const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;
pub const CHUNK_TYPE_CRC32: u16 = 0xCAC4;

pub const SPARSE_HEADER_SIZE: usize = 28;
pub const CHUNK_HEADER_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct SparseHeaderRaw {
    pub magic: U32,
    pub major_version: U16,
    pub minor_version: U16,
    pub file_hdr_sz: U16,
    pub chunk_hdr_sz: U16,
    pub blk_sz: U32,
    pub total_blks: U32,
    pub total_chunks: U32,
    pub image_checksum: U32,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct ChunkHeaderRaw {
    pub chunk_type: U16,
    pub reserved1: U16,
    pub chunk_sz: U32,
    pub total_sz: U32,
}

const _: () = assert!(std::mem::size_of::<SparseHeaderRaw>() == SPARSE_HEADER_SIZE);
const _: () = assert!(std::mem::size_of::<ChunkHeaderRaw>() == CHUNK_HEADER_SIZE);

impl SparseHeaderRaw {
    pub fn new(block_size: u32, total_blocks: u32, total_chunks: u32, image_checksum: u32) -> Self {
        Self {
            magic: U32::new(SPARSE_HEADER_MAGIC),
            major_version: U16::new(SPARSE_HEADER_MAJOR_VERSION),
            minor_version: U16::new(SPARSE_HEADER_MINOR_VERSION),
            file_hdr_sz: U16::new(SPARSE_HEADER_SIZE as u16),
            chunk_hdr_sz: U16::new(CHUNK_HEADER_SIZE as u16),
            blk_sz: U32::new(block_size),
            total_blks: U32::new(total_blocks),
            total_chunks: U32::new(total_chunks),
            image_checksum: U32::new(image_checksum),
        }
    }
}

impl ChunkHeaderRaw {
    pub fn new(chunk_type: u16, chunk_blocks: u32, total_bytes: u32) -> Self {
        Self {
            chunk_type: U16::new(chunk_type),
            reserved1: U16::new(0),
            chunk_sz: U32::new(chunk_blocks),
            total_sz: U32::new(total_bytes),
        }
    }
}
