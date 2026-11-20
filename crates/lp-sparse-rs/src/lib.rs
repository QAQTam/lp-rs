pub mod error;
pub mod format;
pub mod reader;
pub mod writer;

pub use error::{Result, SparseError};
pub use format::{
    CHUNK_HEADER_SIZE, CHUNK_TYPE_CRC32, CHUNK_TYPE_DONT_CARE, CHUNK_TYPE_FILL, CHUNK_TYPE_RAW,
    ChunkHeaderRaw, SPARSE_HEADER_MAGIC, SPARSE_HEADER_MAJOR_VERSION, SPARSE_HEADER_MINOR_VERSION,
    SPARSE_HEADER_SIZE, SparseHeaderRaw,
};
pub use reader::{SparseChunk, SparseReader, is_sparse_image, read_sparse_header};
pub use writer::{SparseBuilder, encode_raw_to_sparse, uniform_fill_u32};
