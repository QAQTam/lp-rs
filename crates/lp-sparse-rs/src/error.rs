use thiserror::Error;

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!(
            "{bytes} bytes ({:.2} GiB)",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        )
    } else if bytes >= 1024 * 1024 {
        format!("{bytes} bytes ({:.2} MiB)", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{bytes} bytes ({:.2} KiB)", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Errors returned when parsing, validating, or writing Android sparse images.
#[derive(Debug, Error)]
pub enum SparseError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "not a valid Android sparse image: found magic 0x{found:08x}, expected 0x{expected:08x} ({hint})"
    )]
    InvalidMagic {
        found: u32,
        expected: u32,
        hint: &'static str,
    },

    #[error("unsupported sparse image major version {major} (max supported: {max_supported})")]
    UnsupportedVersion { major: u16, max_supported: u16 },

    #[error("invalid sparse header: {0}")]
    InvalidHeader(String),

    #[error("invalid sparse chunk #{index} at file offset 0x{offset:x} ({offset}): {reason}")]
    InvalidChunk {
        index: u32,
        offset: u64,
        reason: String,
    },

    #[error("sparse CRC32 mismatch: expected 0x{expected:08x}, actual 0x{actual:08x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error(
        "sparse region [{start_block}..{end_block}) overlaps existing region [{existing_start}..{existing_end})"
    )]
    OverlappingRegion {
        start_block: u64,
        end_block: u64,
        existing_start: u64,
        existing_end: u64,
    },

    #[error(
        "sparse region [{start_block}..{end_block}) exceeds total image size ({total_blocks} blocks = {}, block_size={block_size})",
        format_bytes(total_blocks.saturating_mul(*block_size as u64))
    )]
    OutOfBounds {
        start_block: u64,
        end_block: u64,
        total_blocks: u64,
        block_size: u32,
    },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, SparseError>;
