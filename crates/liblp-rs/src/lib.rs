pub mod builder;
pub mod error;
pub mod format;
pub mod reader;
pub mod utility;
pub mod writer;

pub use builder::{Extent, Interval, MetadataBuilder, PartitionBuilder, PartitionGroup};
pub use error::{LpError, Result};
pub use format::{
    BlockDeviceInfo, LP_BLOCK_DEVICE_SLOT_SUFFIXED, LP_DEFAULT_GROUP,
    LP_DEFAULT_PARTITION_ALIGNMENT, LP_GROUP_SLOT_SUFFIXED, LP_HEADER_FLAG_OVERLAYS_ACTIVE,
    LP_HEADER_FLAG_VIRTUAL_AB_DEVICE, LP_METADATA_DEFAULT_PARTITION_NAME,
    LP_METADATA_GEOMETRY_MAGIC, LP_METADATA_GEOMETRY_SIZE, LP_METADATA_HEADER_MAGIC,
    LP_METADATA_MAJOR_VERSION, LP_METADATA_MINOR_VERSION_MAX, LP_METADATA_MINOR_VERSION_MIN,
    LP_PARTITION_ATTR_DISABLED, LP_PARTITION_ATTR_NONE, LP_PARTITION_ATTR_READONLY,
    LP_PARTITION_ATTR_SLOT_SUFFIXED, LP_PARTITION_ATTR_UPDATED, LP_PARTITION_RESERVED_BYTES,
    LP_SECTOR_SIZE, LP_TARGET_TYPE_LINEAR, LP_TARGET_TYPE_ZERO, LpMetadata, LpMetadataBlockDevice,
    LpMetadataExtent, LpMetadataGeometry, LpMetadataHeader, LpMetadataPartition,
    LpMetadataPartitionGroup, LpMetadataTableDescriptor,
};
pub use reader::{
    is_empty_super_image, parse_geometry, parse_metadata, read_backup_metadata,
    read_from_image_file, read_logical_partition_geometry, read_metadata,
    read_metadata_from_reader, read_primary_metadata,
};
pub use utility::{
    align_to, compute_partition_size, get_backup_geometry_offset, get_backup_metadata_offset,
    get_metadata_super_block_device, get_partition_slot_suffix, get_primary_geometry_offset,
    get_primary_metadata_offset, get_total_metadata_size, get_total_super_partition_size, sha256,
    slot_suffix_for_slot_number,
};
pub use writer::{
    ImageBuilder, flash_partition_table, serialize_geometry, serialize_metadata,
    update_partition_table, update_sparse_super_image, validate_and_serialize_metadata,
    write_empty_image_file, write_partition_to_raw_super, write_split_image_files,
    write_to_image_file,
};
