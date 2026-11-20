#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use zerocopy::byteorder::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const LP_METADATA_GEOMETRY_MAGIC: u32 = 0x616c_4467;
pub const LP_METADATA_GEOMETRY_SIZE: usize = 4096;
pub const LP_METADATA_HEADER_MAGIC: u32 = 0x414C_5030;

pub const LP_METADATA_MAJOR_VERSION: u16 = 10;
pub const LP_METADATA_MINOR_VERSION_MIN: u16 = 0;
pub const LP_METADATA_MINOR_VERSION_MAX: u16 = 2;

pub const LP_METADATA_VERSION_FOR_UPDATED_ATTR: u16 = 1;
pub const LP_METADATA_VERSION_FOR_EXPANDED_HEADER: u16 = 2;

pub const LP_PARTITION_ATTR_NONE: u32 = 0x0;
pub const LP_PARTITION_ATTR_READONLY: u32 = 1 << 0;
pub const LP_PARTITION_ATTR_SLOT_SUFFIXED: u32 = 1 << 1;
pub const LP_PARTITION_ATTR_UPDATED: u32 = 1 << 2;
pub const LP_PARTITION_ATTR_DISABLED: u32 = 1 << 3;

pub const LP_PARTITION_ATTRIBUTE_MASK_V0: u32 =
    LP_PARTITION_ATTR_READONLY | LP_PARTITION_ATTR_SLOT_SUFFIXED;
pub const LP_PARTITION_ATTRIBUTE_MASK_V1: u32 =
    LP_PARTITION_ATTR_UPDATED | LP_PARTITION_ATTR_DISABLED;
pub const LP_PARTITION_ATTRIBUTE_MASK: u32 =
    LP_PARTITION_ATTRIBUTE_MASK_V0 | LP_PARTITION_ATTRIBUTE_MASK_V1;

pub const LP_METADATA_DEFAULT_PARTITION_NAME: &str = "super";
pub const LP_DEFAULT_GROUP: &str = "default";
pub const LP_SECTOR_SIZE: u64 = 512;
pub const LP_PARTITION_RESERVED_BYTES: u64 = 4096;
pub const LP_DEFAULT_PARTITION_ALIGNMENT: u32 = 1024 * 1024;

pub const LP_HEADER_FLAG_VIRTUAL_AB_DEVICE: u32 = 0x1;
pub const LP_HEADER_FLAG_OVERLAYS_ACTIVE: u32 = 0x2;

pub const LP_TARGET_TYPE_LINEAR: u32 = 0;
pub const LP_TARGET_TYPE_ZERO: u32 = 1;

pub const LP_GROUP_SLOT_SUFFIXED: u32 = 1 << 0;
pub const LP_BLOCK_DEVICE_SLOT_SUFFIXED: u32 = 1 << 0;

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataGeometryRaw {
    pub magic: U32,
    pub struct_size: U32,
    pub checksum: [u8; 32],
    pub metadata_max_size: U32,
    pub metadata_slot_count: U32,
    pub logical_block_size: U32,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataTableDescriptorRaw {
    pub offset: U32,
    pub num_entries: U32,
    pub entry_size: U32,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataHeaderV1_0Raw {
    pub magic: U32,
    pub major_version: U16,
    pub minor_version: U16,
    pub header_size: U32,
    pub header_checksum: [u8; 32],
    pub tables_size: U32,
    pub tables_checksum: [u8; 32],
    pub partitions: LpMetadataTableDescriptorRaw,
    pub extents: LpMetadataTableDescriptorRaw,
    pub groups: LpMetadataTableDescriptorRaw,
    pub block_devices: LpMetadataTableDescriptorRaw,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataHeaderRaw {
    pub magic: U32,
    pub major_version: U16,
    pub minor_version: U16,
    pub header_size: U32,
    pub header_checksum: [u8; 32],
    pub tables_size: U32,
    pub tables_checksum: [u8; 32],
    pub partitions: LpMetadataTableDescriptorRaw,
    pub extents: LpMetadataTableDescriptorRaw,
    pub groups: LpMetadataTableDescriptorRaw,
    pub block_devices: LpMetadataTableDescriptorRaw,
    pub flags: U32,
    pub reserved: [u8; 124],
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataPartitionRaw {
    pub name: [u8; 36],
    pub attributes: U32,
    pub first_extent_index: U32,
    pub num_extents: U32,
    pub group_index: U32,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataExtentRaw {
    pub num_sectors: U64,
    pub target_type: U32,
    pub target_data: U64,
    pub target_source: U32,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataPartitionGroupRaw {
    pub name: [u8; 36],
    pub flags: U32,
    pub maximum_size: U64,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub struct LpMetadataBlockDeviceRaw {
    pub first_logical_sector: U64,
    pub alignment: U32,
    pub alignment_offset: U32,
    pub size: U64,
    pub partition_name: [u8; 36],
    pub flags: U32,
}

const _: () = assert!(std::mem::size_of::<LpMetadataGeometryRaw>() == 52);
const _: () = assert!(std::mem::size_of::<LpMetadataTableDescriptorRaw>() == 12);
const _: () = assert!(std::mem::size_of::<LpMetadataHeaderV1_0Raw>() == 128);
const _: () = assert!(std::mem::size_of::<LpMetadataHeaderRaw>() == 256);
const _: () = assert!(std::mem::size_of::<LpMetadataPartitionRaw>() == 52);
const _: () = assert!(std::mem::size_of::<LpMetadataExtentRaw>() == 24);
const _: () = assert!(std::mem::size_of::<LpMetadataPartitionGroupRaw>() == 48);
const _: () = assert!(std::mem::size_of::<LpMetadataBlockDeviceRaw>() == 64);

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataGeometry {
    pub magic: u32,
    pub struct_size: u32,
    pub checksum: [u8; 32],
    pub metadata_max_size: u32,
    pub metadata_slot_count: u32,
    pub logical_block_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataTableDescriptor {
    pub offset: u32,
    pub num_entries: u32,
    pub entry_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataHeader {
    pub magic: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub header_size: u32,
    pub header_checksum: [u8; 32],
    pub tables_size: u32,
    pub tables_checksum: [u8; 32],
    pub partitions: LpMetadataTableDescriptor,
    pub extents: LpMetadataTableDescriptor,
    pub groups: LpMetadataTableDescriptor,
    pub block_devices: LpMetadataTableDescriptor,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataPartition {
    pub name: String,
    pub attributes: u32,
    pub first_extent_index: u32,
    pub num_extents: u32,
    pub group_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataExtent {
    pub num_sectors: u64,
    pub target_type: u32,
    pub target_data: u64,
    pub target_source: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataPartitionGroup {
    pub name: String,
    pub flags: u32,
    pub maximum_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadataBlockDevice {
    pub first_logical_sector: u64,
    pub alignment: u32,
    pub alignment_offset: u32,
    pub size: u64,
    pub partition_name: String,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LpMetadata {
    pub geometry: LpMetadataGeometry,
    pub header: LpMetadataHeader,
    pub partitions: Vec<LpMetadataPartition>,
    pub extents: Vec<LpMetadataExtent>,
    pub groups: Vec<LpMetadataPartitionGroup>,
    pub block_devices: Vec<LpMetadataBlockDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDeviceInfo {
    pub partition_name: String,
    pub size: u64,
    pub alignment: u32,
    pub alignment_offset: u32,
    pub logical_block_size: u32,
}

impl BlockDeviceInfo {
    pub fn new(
        partition_name: impl Into<String>,
        size: u64,
        alignment: u32,
        alignment_offset: u32,
        logical_block_size: u32,
    ) -> Self {
        Self {
            partition_name: partition_name.into(),
            size,
            alignment,
            alignment_offset,
            logical_block_size,
        }
    }
}

pub fn decode_fixed_str(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

pub fn encode_fixed_str<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let bytes = s.as_bytes();
    if bytes.len() > N {
        return Err(format!("name '{s}' exceeds maximum length {N}"));
    }
    let mut out = [0u8; N];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}
