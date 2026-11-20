use lp_sparse::error::format_bytes;
use thiserror::Error;

pub fn hex_prefix(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(16);
    for b in &hash[..8] {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

#[derive(Debug, Error)]
pub enum LpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sparse image error: {0}")]
    Sparse(#[from] lp_sparse::SparseError),

    #[error(
        "invalid LpMetadataGeometry magic at offset 0x{offset:x} ({offset}): found 0x{found:08x}, expected 0x{expected:08x} ({hint})"
    )]
    InvalidGeometryMagic {
        offset: u64,
        found: u32,
        expected: u32,
        hint: &'static str,
    },

    #[error(
        "LpMetadataGeometry SHA-256 checksum mismatch at offset 0x{offset:x}: expected {expected_prefix}..., actual {actual_prefix}..."
    )]
    GeometryChecksumMismatch {
        offset: u64,
        expected_prefix: String,
        actual_prefix: String,
    },

    #[error(
        "failed to read LpMetadataGeometry (both primary and backup corrupted)\n  primary: {primary}\n  backup:  {backup}"
    )]
    GeometryReadFailed {
        primary: Box<LpError>,
        backup: Box<LpError>,
    },

    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),

    #[error(
        "invalid LpMetadataHeader magic at offset 0x{offset:x} ({offset}): found 0x{found:08x}, expected 0x{expected:08x} ({hint})"
    )]
    InvalidMetadataMagic {
        offset: u64,
        found: u32,
        expected: u32,
        hint: &'static str,
    },

    #[error(
        "incompatible metadata version {major}.{minor} (expected major={supported_major}, minor <= {max_minor})"
    )]
    UnsupportedMetadataVersion {
        major: u16,
        minor: u16,
        supported_major: u16,
        max_minor: u16,
    },

    #[error(
        "metadata header SHA-256 checksum mismatch: expected {expected_prefix}..., actual {actual_prefix}..."
    )]
    MetadataHeaderChecksumMismatch {
        expected_prefix: String,
        actual_prefix: String,
    },

    #[error(
        "metadata tables SHA-256 checksum mismatch: expected {expected_prefix}..., actual {actual_prefix}..."
    )]
    MetadataTablesChecksumMismatch {
        expected_prefix: String,
        actual_prefix: String,
    },

    #[error(
        "failed to read metadata for slot {slot} (both primary and backup failed)\n  primary: {primary}\n  backup:  {backup}"
    )]
    MetadataReadFailed {
        slot: u32,
        primary: Box<LpError>,
        backup: Box<LpError>,
    },

    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    #[error(
        "partition '{partition}' cannot fit in group '{group}': requested {}, group has {} used out of {} (remaining {}, short by {})",
        format_bytes(*requested),
        format_bytes(*used),
        format_bytes(*maximum),
        format_bytes(*remaining),
        format_bytes(*deficit)
    )]
    GroupOutOfSpace {
        partition: String,
        group: String,
        requested: u64,
        used: u64,
        maximum: u64,
        remaining: u64,
        deficit: u64,
    },

    #[error(
        "not enough space in super device for partition '{partition}': requested {}, total allocatable {}, used {}, free {}, short by {}",
        format_bytes(*requested),
        format_bytes(*allocatable),
        format_bytes(*used),
        format_bytes(*free),
        format_bytes(*deficit)
    )]
    SuperOutOfSpace {
        partition: String,
        requested: u64,
        allocatable: u64,
        used: u64,
        free: u64,
        deficit: u64,
    },

    #[error(
        "image '{image_path}' ({}) exceeds partition '{partition}' capacity ({}), overflow by {}",
        format_bytes(*image_size),
        format_bytes(*partition_size),
        format_bytes(*overflow)
    )]
    ImageExceedsPartition {
        image_path: String,
        image_size: u64,
        partition: String,
        partition_size: u64,
        overflow: u64,
    },

    #[error(
        "serialized metadata ({}) exceeds metadata_max_size ({}); increase --metadata-size to at least {suggested_max_size}",
        format_bytes(*serialized_size as u64),
        format_bytes(*metadata_max_size as u64)
    )]
    MetadataTooLarge {
        serialized_size: usize,
        metadata_max_size: u32,
        suggested_max_size: u32,
    },

    #[error(
        "{field} ({value}) is not aligned to {alignment}; nearest aligned value >= {value} is {suggested}"
    )]
    AlignmentError {
        field: String,
        value: u64,
        alignment: u64,
        suggested: u64,
    },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("out of space: {0}")]
    OutOfSpace(String),

    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, LpError>;
