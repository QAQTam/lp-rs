use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use lp_sparse::{SparseReader, is_sparse_image};
use zerocopy::{FromBytes, IntoBytes};

use crate::error::{LpError, Result, hex_prefix};
use crate::format::{
    LP_BLOCK_DEVICE_SLOT_SUFFIXED, LP_GROUP_SLOT_SUFFIXED, LP_METADATA_GEOMETRY_MAGIC,
    LP_METADATA_GEOMETRY_SIZE, LP_METADATA_HEADER_MAGIC, LP_METADATA_MAJOR_VERSION,
    LP_METADATA_MINOR_VERSION_MAX, LP_METADATA_VERSION_FOR_EXPANDED_HEADER,
    LP_METADATA_VERSION_FOR_UPDATED_ATTR, LP_PARTITION_ATTR_SLOT_SUFFIXED,
    LP_PARTITION_ATTRIBUTE_MASK_V0, LP_PARTITION_ATTRIBUTE_MASK_V1, LP_SECTOR_SIZE,
    LP_TARGET_TYPE_LINEAR, LpMetadata, LpMetadataBlockDevice, LpMetadataBlockDeviceRaw,
    LpMetadataExtent, LpMetadataExtentRaw, LpMetadataGeometry, LpMetadataGeometryRaw,
    LpMetadataHeader, LpMetadataHeaderRaw, LpMetadataHeaderV1_0Raw, LpMetadataPartition,
    LpMetadataPartitionGroup, LpMetadataPartitionGroupRaw, LpMetadataPartitionRaw,
    LpMetadataTableDescriptor, LpMetadataTableDescriptorRaw, decode_fixed_str,
};
use crate::utility::{
    get_backup_geometry_offset, get_backup_metadata_offset, get_metadata_super_block_device,
    get_primary_geometry_offset, get_primary_metadata_offset, get_total_metadata_size, sha256,
    slot_suffix_for_slot_number,
};

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

fn open_raw_or_sparse(path: &Path) -> Result<Box<dyn ReadSeek>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    if is_sparse_image(&mut reader)? {
        let sparse = SparseReader::open(reader)?;
        Ok(Box::new(sparse))
    } else {
        Ok(Box::new(reader))
    }
}

fn geometry_magic_hint(found: u32) -> &'static str {
    match found {
        0x0000_0000 => "all-zero bytes (blank, unformatted, or wiped super partition)",
        0xed26_ff3a => "Android sparse magic; sparse container should be decoded first",
        0x414C_5030 => "looks like LpMetadataHeader instead of LpMetadataGeometry",
        0x5452_4150 => "looks like a GPT header ('PART'); check partition offset",
        _ => "not a valid dynamic partition super image or corrupted geometry block",
    }
}

fn metadata_magic_hint(found: u32) -> &'static str {
    match found {
        0x0000_0000 => "all-zero bytes (empty or wiped metadata slot)",
        0x616c_4467 => "found LpMetadataGeometry magic instead of LpMetadataHeader",
        0xed26_ff3a => "found Android sparse magic",
        _ => "corrupted metadata header or wrong slot offset",
    }
}

/// Parses and validates an `LpMetadataGeometry` from a `LP_METADATA_GEOMETRY_SIZE`-byte buffer.
pub fn parse_geometry(buffer: &[u8]) -> Result<LpMetadataGeometry> {
    parse_geometry_at_offset(buffer, 0)
}

fn parse_geometry_at_offset(buffer: &[u8], offset: u64) -> Result<LpMetadataGeometry> {
    let raw_size = std::mem::size_of::<LpMetadataGeometryRaw>();
    if buffer.len() < raw_size {
        return Err(LpError::InvalidGeometry(format!(
            "buffer length ({}) smaller than LpMetadataGeometry ({raw_size} bytes)",
            buffer.len()
        )));
    }

    let raw = LpMetadataGeometryRaw::read_from_bytes(&buffer[..raw_size])
        .map_err(|_| LpError::InvalidGeometry("failed to decode geometry".into()))?;

    let magic = raw.magic.get();
    if magic != LP_METADATA_GEOMETRY_MAGIC {
        return Err(LpError::InvalidGeometryMagic {
            offset,
            found: magic,
            expected: LP_METADATA_GEOMETRY_MAGIC,
            hint: geometry_magic_hint(magic),
        });
    }

    let struct_size = raw.struct_size.get() as usize;
    if struct_size > raw_size {
        return Err(LpError::InvalidGeometry(format!(
            "unrecognized geometry struct_size ({struct_size} > {raw_size})"
        )));
    }

    let mut temp = raw;
    temp.checksum = [0u8; 32];
    let computed = sha256(&temp.as_bytes()[..struct_size]);
    if computed != raw.checksum {
        return Err(LpError::GeometryChecksumMismatch {
            offset,
            expected_prefix: hex_prefix(&raw.checksum),
            actual_prefix: hex_prefix(&computed),
        });
    }

    if struct_size != raw_size {
        return Err(LpError::InvalidGeometry(format!(
            "unexpected geometry struct_size ({struct_size} != {raw_size})"
        )));
    }

    let metadata_slot_count = raw.metadata_slot_count.get();
    if metadata_slot_count == 0 {
        return Err(LpError::InvalidGeometry(
            "metadata_slot_count cannot be 0".into(),
        ));
    }

    let metadata_max_size = raw.metadata_max_size.get();
    if metadata_max_size as u64 % LP_SECTOR_SIZE != 0 {
        return Err(LpError::InvalidGeometry(format!(
            "metadata_max_size ({metadata_max_size}) is not sector-aligned (multiple of {LP_SECTOR_SIZE})"
        )));
    }

    Ok(LpMetadataGeometry {
        magic,
        struct_size: struct_size as u32,
        checksum: raw.checksum,
        metadata_max_size,
        metadata_slot_count,
        logical_block_size: raw.logical_block_size.get(),
    })
}

fn read_geometry_at<R: Read + Seek>(reader: &mut R, offset: u64) -> Result<LpMetadataGeometry> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = [0u8; LP_METADATA_GEOMETRY_SIZE];
    reader.read_exact(&mut buf)?;
    parse_geometry_at_offset(&buf, offset)
}

/// Reads and validates the logical partition geometry from primary or backup slot.
pub fn read_logical_partition_geometry<R: Read + Seek>(
    reader: &mut R,
) -> Result<LpMetadataGeometry> {
    match read_geometry_at(reader, get_primary_geometry_offset()) {
        Ok(g) => Ok(g),
        Err(primary_err) => match read_geometry_at(reader, get_backup_geometry_offset()) {
            Ok(g) => Ok(g),
            Err(backup_err) => Err(LpError::GeometryReadFailed {
                primary: Box::new(primary_err),
                backup: Box::new(backup_err),
            }),
        },
    }
}

fn validate_table_bounds(tables_size: u32, table: &LpMetadataTableDescriptor) -> bool {
    if table.offset > tables_size {
        return false;
    }
    let Some(byte_len) = (table.num_entries as u64).checked_mul(table.entry_size as u64) else {
        return false;
    };
    (tables_size - table.offset) as u64 >= byte_len
}

fn decode_descriptor(raw: &LpMetadataTableDescriptorRaw) -> LpMetadataTableDescriptor {
    LpMetadataTableDescriptor {
        offset: raw.offset.get(),
        num_entries: raw.num_entries.get(),
        entry_size: raw.entry_size.get(),
    }
}

/// Parses and validates `LpMetadata` starting at the current position of `reader`.
pub fn parse_metadata<R: Read>(
    geometry: &LpMetadataGeometry,
    reader: &mut R,
) -> Result<LpMetadata> {
    parse_metadata_at(geometry, reader, 0)
}

fn parse_metadata_at<R: Read>(
    geometry: &LpMetadataGeometry,
    reader: &mut R,
    header_offset: u64,
) -> Result<LpMetadata> {
    let v1_0_size = std::mem::size_of::<LpMetadataHeaderV1_0Raw>();
    let full_header_size = std::mem::size_of::<LpMetadataHeaderRaw>();
    let mut header_bytes = [0u8; 256];

    reader.read_exact(&mut header_bytes[..v1_0_size])?;
    let hdr_v0 = LpMetadataHeaderV1_0Raw::read_from_bytes(&header_bytes[..v1_0_size])
        .map_err(|_| LpError::InvalidMetadata("failed to decode metadata header".into()))?;

    let magic = hdr_v0.magic.get();
    if magic != LP_METADATA_HEADER_MAGIC {
        return Err(LpError::InvalidMetadataMagic {
            offset: header_offset,
            found: magic,
            expected: LP_METADATA_HEADER_MAGIC,
            hint: metadata_magic_hint(magic),
        });
    }
    let major_version = hdr_v0.major_version.get();
    let minor_version = hdr_v0.minor_version.get();
    if major_version != LP_METADATA_MAJOR_VERSION || minor_version > LP_METADATA_MINOR_VERSION_MAX {
        return Err(LpError::UnsupportedMetadataVersion {
            major: major_version,
            minor: minor_version,
            supported_major: LP_METADATA_MAJOR_VERSION,
            max_minor: LP_METADATA_MINOR_VERSION_MAX,
        });
    }

    let expected_header_size = if minor_version < LP_METADATA_VERSION_FOR_EXPANDED_HEADER {
        v1_0_size
    } else {
        full_header_size
    };
    let header_size = hdr_v0.header_size.get() as usize;
    if header_size != expected_header_size {
        return Err(LpError::InvalidMetadata(format!(
            "invalid header_size ({header_size} != {expected_header_size}) for version {major_version}.{minor_version}"
        )));
    }

    if header_size > v1_0_size {
        reader.read_exact(&mut header_bytes[v1_0_size..header_size])?;
    }

    let hdr_raw = LpMetadataHeaderRaw::read_from_bytes(&header_bytes)
        .map_err(|_| LpError::InvalidMetadata("failed to decode full header".into()))?;

    let mut temp_bytes = header_bytes;
    // Zero out header_checksum (bytes 12..44) before hashing.
    temp_bytes[12..44].fill(0);
    let computed_hdr_hash = sha256(&temp_bytes[..header_size]);
    if computed_hdr_hash != hdr_raw.header_checksum {
        return Err(LpError::MetadataHeaderChecksumMismatch {
            expected_prefix: hex_prefix(&hdr_raw.header_checksum),
            actual_prefix: hex_prefix(&computed_hdr_hash),
        });
    }

    let partitions_desc = decode_descriptor(&hdr_raw.partitions);
    let extents_desc = decode_descriptor(&hdr_raw.extents);
    let groups_desc = decode_descriptor(&hdr_raw.groups);
    let block_devices_desc = decode_descriptor(&hdr_raw.block_devices);
    let tables_size = hdr_raw.tables_size.get();

    if !validate_table_bounds(tables_size, &partitions_desc)
        || !validate_table_bounds(tables_size, &extents_desc)
        || !validate_table_bounds(tables_size, &groups_desc)
        || !validate_table_bounds(tables_size, &block_devices_desc)
    {
        return Err(LpError::InvalidMetadata(
            "invalid metadata table bounds".into(),
        ));
    }

    if partitions_desc.entry_size as usize != std::mem::size_of::<LpMetadataPartitionRaw>() {
        return Err(LpError::InvalidMetadata(
            "invalid partition table entry_size".into(),
        ));
    }
    if extents_desc.entry_size as usize != std::mem::size_of::<LpMetadataExtentRaw>() {
        return Err(LpError::InvalidMetadata(
            "invalid extent table entry_size".into(),
        ));
    }
    if groups_desc.entry_size as usize != std::mem::size_of::<LpMetadataPartitionGroupRaw>() {
        return Err(LpError::InvalidMetadata(
            "invalid group table entry_size".into(),
        ));
    }
    if block_devices_desc.entry_size as usize != std::mem::size_of::<LpMetadataBlockDeviceRaw>() {
        return Err(LpError::InvalidMetadata(
            "invalid block_device table entry_size".into(),
        ));
    }

    if tables_size > geometry.metadata_max_size {
        return Err(LpError::InvalidMetadata(format!(
            "tables_size ({tables_size}) exceeds metadata_max_size ({})",
            geometry.metadata_max_size
        )));
    }

    let mut table_buf = vec![0u8; tables_size as usize];
    reader.read_exact(&mut table_buf)?;

    let computed_tables_hash = sha256(&table_buf);
    if computed_tables_hash != hdr_raw.tables_checksum {
        return Err(LpError::MetadataTablesChecksumMismatch {
            expected_prefix: hex_prefix(&hdr_raw.tables_checksum),
            actual_prefix: hex_prefix(&computed_tables_hash),
        });
    }

    let mut valid_attributes = LP_PARTITION_ATTRIBUTE_MASK_V0;
    if minor_version >= LP_METADATA_VERSION_FOR_UPDATED_ATTR {
        valid_attributes |= LP_PARTITION_ATTRIBUTE_MASK_V1;
    }

    let mut partitions = Vec::with_capacity(partitions_desc.num_entries as usize);
    let mut cursor = partitions_desc.offset as usize;
    for _ in 0..partitions_desc.num_entries {
        let raw = LpMetadataPartitionRaw::read_from_bytes(
            &table_buf[cursor..cursor + partitions_desc.entry_size as usize],
        )
        .map_err(|_| LpError::InvalidMetadata("failed to decode partition entry".into()))?;
        cursor += partitions_desc.entry_size as usize;

        let attributes = raw.attributes.get();
        if attributes & !valid_attributes != 0 {
            return Err(LpError::InvalidMetadata(format!(
                "partition has invalid attributes: 0x{attributes:x}"
            )));
        }
        let first_extent_index = raw.first_extent_index.get();
        let num_extents = raw.num_extents.get();
        let end_extent = first_extent_index
            .checked_add(num_extents)
            .ok_or_else(|| LpError::InvalidMetadata("extent index overflow".into()))?;
        if end_extent > extents_desc.num_entries {
            return Err(LpError::InvalidMetadata(
                "partition extent range exceeds extent table".into(),
            ));
        }
        let group_index = raw.group_index.get();
        if group_index >= groups_desc.num_entries {
            return Err(LpError::InvalidMetadata(
                "partition group_index out of range".into(),
            ));
        }

        partitions.push(LpMetadataPartition {
            name: decode_fixed_str(&raw.name),
            attributes,
            first_extent_index,
            num_extents,
            group_index,
        });
    }

    let mut extents = Vec::with_capacity(extents_desc.num_entries as usize);
    cursor = extents_desc.offset as usize;
    for _ in 0..extents_desc.num_entries {
        let raw = LpMetadataExtentRaw::read_from_bytes(
            &table_buf[cursor..cursor + extents_desc.entry_size as usize],
        )
        .map_err(|_| LpError::InvalidMetadata("failed to decode extent entry".into()))?;
        cursor += extents_desc.entry_size as usize;

        let target_type = raw.target_type.get();
        let target_source = raw.target_source.get();
        if target_type == LP_TARGET_TYPE_LINEAR && target_source >= block_devices_desc.num_entries {
            return Err(LpError::InvalidMetadata(
                "extent references invalid block device index".into(),
            ));
        }

        extents.push(LpMetadataExtent {
            num_sectors: raw.num_sectors.get(),
            target_type,
            target_data: raw.target_data.get(),
            target_source,
        });
    }

    let mut groups = Vec::with_capacity(groups_desc.num_entries as usize);
    cursor = groups_desc.offset as usize;
    for _ in 0..groups_desc.num_entries {
        let raw = LpMetadataPartitionGroupRaw::read_from_bytes(
            &table_buf[cursor..cursor + groups_desc.entry_size as usize],
        )
        .map_err(|_| LpError::InvalidMetadata("failed to decode group entry".into()))?;
        cursor += groups_desc.entry_size as usize;

        groups.push(LpMetadataPartitionGroup {
            name: decode_fixed_str(&raw.name),
            flags: raw.flags.get(),
            maximum_size: raw.maximum_size.get(),
        });
    }

    let mut block_devices = Vec::with_capacity(block_devices_desc.num_entries as usize);
    cursor = block_devices_desc.offset as usize;
    for _ in 0..block_devices_desc.num_entries {
        let raw = LpMetadataBlockDeviceRaw::read_from_bytes(
            &table_buf[cursor..cursor + block_devices_desc.entry_size as usize],
        )
        .map_err(|_| LpError::InvalidMetadata("failed to decode block device entry".into()))?;
        cursor += block_devices_desc.entry_size as usize;

        block_devices.push(LpMetadataBlockDevice {
            first_logical_sector: raw.first_logical_sector.get(),
            alignment: raw.alignment.get(),
            alignment_offset: raw.alignment_offset.get(),
            size: raw.size.get(),
            partition_name: decode_fixed_str(&raw.partition_name),
            flags: raw.flags.get(),
        });
    }

    let metadata = LpMetadata {
        geometry: geometry.clone(),
        header: LpMetadataHeader {
            magic,
            major_version,
            minor_version,
            header_size: header_size as u32,
            header_checksum: hdr_raw.header_checksum,
            tables_size,
            tables_checksum: hdr_raw.tables_checksum,
            partitions: partitions_desc,
            extents: extents_desc,
            groups: groups_desc,
            block_devices: block_devices_desc,
            flags: hdr_raw.flags.get(),
        },
        partitions,
        extents,
        groups,
        block_devices,
    };

    let super_device = get_metadata_super_block_device(&metadata)
        .ok_or_else(|| LpError::InvalidMetadata("metadata has no super block device".into()))?;
    let metadata_region =
        get_total_metadata_size(geometry.metadata_max_size, geometry.metadata_slot_count);
    if metadata_region > super_device.first_logical_sector * LP_SECTOR_SIZE {
        return Err(LpError::InvalidMetadata(
            "metadata region overlaps logical partitions".into(),
        ));
    }

    Ok(metadata)
}

fn adjust_metadata_for_slot(metadata: &mut LpMetadata, slot_number: u32) -> Result<()> {
    let suffix = slot_suffix_for_slot_number(slot_number)?;
    for part in &mut metadata.partitions {
        if part.attributes & LP_PARTITION_ATTR_SLOT_SUFFIXED != 0 {
            let new_name = format!("{}{}", part.name, suffix);
            if new_name.len() > 36 {
                return Err(LpError::InvalidMetadata(format!(
                    "partition name too long after suffixing: {new_name}"
                )));
            }
            part.name = new_name;
            part.attributes &= !LP_PARTITION_ATTR_SLOT_SUFFIXED;
        }
    }
    for dev in &mut metadata.block_devices {
        if dev.flags & LP_BLOCK_DEVICE_SLOT_SUFFIXED != 0 {
            let new_name = format!("{}{}", dev.partition_name, suffix);
            if new_name.len() > 36 {
                return Err(LpError::InvalidMetadata(format!(
                    "block device name too long after suffixing: {new_name}"
                )));
            }
            dev.partition_name = new_name;
            dev.flags &= !LP_BLOCK_DEVICE_SLOT_SUFFIXED;
        }
    }
    for group in &mut metadata.groups {
        if group.flags & LP_GROUP_SLOT_SUFFIXED != 0 {
            let new_name = format!("{}{}", group.name, suffix);
            if new_name.len() > 36 {
                return Err(LpError::InvalidMetadata(format!(
                    "group name too long after suffixing: {new_name}"
                )));
            }
            group.name = new_name;
            group.flags &= !LP_GROUP_SLOT_SUFFIXED;
        }
    }
    Ok(())
}

/// Checks whether a file starts directly with `LP_METADATA_GEOMETRY_MAGIC` (i.e. `super_empty.img`).
pub fn is_empty_super_image(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => Ok(u32::from_le_bytes(magic) == LP_METADATA_GEOMETRY_MAGIC),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(LpError::Io(e)),
    }
}

/// Reads metadata from a minimal `super_empty.img` file (geometry at offset 0, followed by metadata).
pub fn read_from_image_file(path: &Path) -> Result<LpMetadata> {
    let mut file = BufReader::new(File::open(path)?);
    let mut buf = [0u8; LP_METADATA_GEOMETRY_SIZE];
    file.read_exact(&mut buf)?;
    let geometry = parse_geometry(&buf)?;
    parse_metadata(&geometry, &mut file)
}

/// Reads metadata from a full `super.img` (raw OR Android sparse!) at `slot_number`,
/// or automatically falls back to `super_empty.img` format if geometry is at offset 0.
pub fn read_metadata(path: &Path, slot_number: u32) -> Result<LpMetadata> {
    if is_empty_super_image(path)? {
        let mut meta = read_from_image_file(path)?;
        adjust_metadata_for_slot(&mut meta, slot_number)?;
        return Ok(meta);
    }

    let mut reader = open_raw_or_sparse(path)?;
    read_metadata_from_reader(&mut reader, slot_number)
}

/// Reads metadata from the primary metadata slot `slot_number`.
pub fn read_primary_metadata<R: Read + Seek>(
    reader: &mut R,
    geometry: &LpMetadataGeometry,
    slot_number: u32,
) -> Result<LpMetadata> {
    let offset = get_primary_metadata_offset(geometry, slot_number);
    reader.seek(SeekFrom::Start(offset))?;
    parse_metadata_at(geometry, reader, offset)
}

/// Reads metadata from the backup metadata slot `slot_number`.
pub fn read_backup_metadata<R: Read + Seek>(
    reader: &mut R,
    geometry: &LpMetadataGeometry,
    slot_number: u32,
) -> Result<LpMetadata> {
    let offset = get_backup_metadata_offset(geometry, slot_number);
    reader.seek(SeekFrom::Start(offset))?;
    parse_metadata_at(geometry, reader, offset)
}

/// Reads metadata from any seekable raw stream at `slot_number`.
pub fn read_metadata_from_reader<R: Read + Seek>(
    reader: &mut R,
    slot_number: u32,
) -> Result<LpMetadata> {
    let geometry = read_logical_partition_geometry(reader)?;
    if slot_number >= geometry.metadata_slot_count {
        return Err(LpError::InvalidArgument(format!(
            "slot_number ({slot_number}) >= metadata_slot_count ({}); valid slots: 0..{}",
            geometry.metadata_slot_count,
            geometry.metadata_slot_count
        )));
    }

    let primary_err = match read_primary_metadata(reader, &geometry, slot_number) {
        Ok(mut metadata) => {
            adjust_metadata_for_slot(&mut metadata, slot_number)?;
            return Ok(metadata);
        }
        Err(e) => e,
    };

    match read_backup_metadata(reader, &geometry, slot_number) {
        Ok(mut metadata) => {
            adjust_metadata_for_slot(&mut metadata, slot_number)?;
            Ok(metadata)
        }
        Err(backup_err) => Err(LpError::MetadataReadFailed {
            slot: slot_number,
            primary: Box::new(primary_err),
            backup: Box::new(backup_err),
        }),
    }
}
