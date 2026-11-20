use sha2::{Digest, Sha256};

use crate::error::{LpError, Result};
use crate::format::{
    LP_METADATA_GEOMETRY_SIZE, LP_PARTITION_RESERVED_BYTES, LP_SECTOR_SIZE, LpMetadata,
    LpMetadataBlockDevice, LpMetadataGeometry, LpMetadataPartition,
};

pub fn align_to(value: u64, alignment: u64) -> Result<u64> {
    if alignment == 0 {
        return Ok(value);
    }
    let rem = value % alignment;
    if rem == 0 {
        return Ok(value);
    }
    value
        .checked_add(alignment - rem)
        .ok_or_else(|| LpError::InvalidArgument(format!("integer overflow aligning {value} to {alignment}")))
}

pub fn get_primary_geometry_offset() -> u64 {
    LP_PARTITION_RESERVED_BYTES
}

pub fn get_backup_geometry_offset() -> u64 {
    get_primary_geometry_offset() + LP_METADATA_GEOMETRY_SIZE as u64
}

pub fn get_primary_metadata_offset(geometry: &LpMetadataGeometry, slot_number: u32) -> u64 {
    LP_PARTITION_RESERVED_BYTES
        + (LP_METADATA_GEOMETRY_SIZE as u64 * 2)
        + (geometry.metadata_max_size as u64) * (slot_number as u64)
}

pub fn get_backup_metadata_offset(geometry: &LpMetadataGeometry, slot_number: u32) -> u64 {
    let start = LP_PARTITION_RESERVED_BYTES
        + (LP_METADATA_GEOMETRY_SIZE as u64 * 2)
        + (geometry.metadata_max_size as u64) * (geometry.metadata_slot_count as u64);
    start + (geometry.metadata_max_size as u64) * (slot_number as u64)
}

pub fn get_total_metadata_size(metadata_max_size: u32, max_slots: u32) -> u64 {
    LP_PARTITION_RESERVED_BYTES
        + ((LP_METADATA_GEOMETRY_SIZE as u64) + (metadata_max_size as u64) * (max_slots as u64)) * 2
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

pub fn slot_suffix_for_slot_number(slot_number: u32) -> Result<&'static str> {
    match slot_number {
        0 => Ok("_a"),
        1 => Ok("_b"),
        _ => Err(LpError::InvalidArgument(format!(
            "unsupported slot number {slot_number} (expected 0 or 1)"
        ))),
    }
}

pub fn get_partition_slot_suffix(name: &str) -> &str {
    if let Some(suffix) = name.strip_prefix("")
        && suffix.len() > 2
    {
        let tail = &suffix[suffix.len() - 2..];
        if tail == "_a" || tail == "_b" {
            return tail;
        }
    }
    ""
}

pub fn get_metadata_super_block_device(metadata: &LpMetadata) -> Option<&LpMetadataBlockDevice> {
    metadata.block_devices.first()
}

pub fn get_total_super_partition_size(metadata: &LpMetadata) -> u64 {
    metadata.block_devices.iter().map(|d| d.size).sum()
}

pub fn compute_partition_size(metadata: &LpMetadata, partition: &LpMetadataPartition) -> u64 {
    let start = partition.first_extent_index as usize;
    let end = start + partition.num_extents as usize;
    metadata.extents[start..end]
        .iter()
        .map(|e| e.num_sectors * LP_SECTOR_SIZE)
        .sum()
}
