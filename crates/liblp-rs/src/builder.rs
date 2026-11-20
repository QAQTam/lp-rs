use std::collections::{BTreeSet, HashMap};

use crate::error::{LpError, Result};
use crate::format::{
    BlockDeviceInfo, LP_BLOCK_DEVICE_SLOT_SUFFIXED, LP_DEFAULT_GROUP, LP_GROUP_SLOT_SUFFIXED,
    LP_HEADER_FLAG_OVERLAYS_ACTIVE, LP_HEADER_FLAG_VIRTUAL_AB_DEVICE, LP_METADATA_MAJOR_VERSION,
    LP_METADATA_MINOR_VERSION_MAX, LP_METADATA_MINOR_VERSION_MIN,
    LP_METADATA_VERSION_FOR_EXPANDED_HEADER, LP_METADATA_VERSION_FOR_UPDATED_ATTR,
    LP_PARTITION_ATTR_SLOT_SUFFIXED, LP_PARTITION_ATTRIBUTE_MASK, LP_PARTITION_ATTRIBUTE_MASK_V1,
    LP_SECTOR_SIZE, LP_TARGET_TYPE_LINEAR, LP_TARGET_TYPE_ZERO, LpMetadata, LpMetadataBlockDevice,
    LpMetadataExtent, LpMetadataGeometry, LpMetadataHeader, LpMetadataHeaderRaw,
    LpMetadataHeaderV1_0Raw, LpMetadataPartition, LpMetadataPartitionGroup,
    LpMetadataTableDescriptor,
};
use crate::utility::{align_to, get_total_metadata_size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    Linear {
        num_sectors: u64,
        device_index: u32,
        physical_sector: u64,
    },
    Zero {
        num_sectors: u64,
    },
}

impl Extent {
    pub fn num_sectors(&self) -> u64 {
        match self {
            Extent::Linear { num_sectors, .. } | Extent::Zero { num_sectors } => *num_sectors,
        }
    }

    pub fn set_num_sectors(&mut self, sectors: u64) {
        match self {
            Extent::Linear { num_sectors, .. } | Extent::Zero { num_sectors } => {
                *num_sectors = sectors;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub device_index: u32,
    pub start: u64,
    pub end: u64,
}

impl Interval {
    pub fn length(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Clone)]
pub struct PartitionBuilder {
    pub name: String,
    pub group_name: String,
    pub attributes: u32,
    pub extents: Vec<Extent>,
    size: u64,
}

impl PartitionBuilder {
    pub fn new(name: impl Into<String>, group_name: impl Into<String>, attributes: u32) -> Self {
        Self {
            name: name.into(),
            group_name: group_name.into(),
            attributes,
            extents: Vec::new(),
            size: 0,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn bytes_on_disk(&self) -> u64 {
        self.extents
            .iter()
            .filter_map(|e| match e {
                Extent::Linear { num_sectors, .. } => Some(num_sectors * LP_SECTOR_SIZE),
                Extent::Zero { .. } => None,
            })
            .sum()
    }

    pub fn add_extent(&mut self, extent: Extent) {
        self.size += extent.num_sectors() * LP_SECTOR_SIZE;
        if let Extent::Linear {
            num_sectors,
            device_index,
            physical_sector,
        } = extent
            && let Some(Extent::Linear {
                num_sectors: prev_sectors,
                device_index: prev_dev,
                physical_sector: prev_phys,
            }) = self.extents.last_mut()
            && *prev_dev == device_index
            && *prev_phys + *prev_sectors == physical_sector
        {
            *prev_sectors += num_sectors;
            return;
        }
        self.extents.push(extent);
    }

    pub fn remove_extents(&mut self) {
        self.size = 0;
        self.extents.clear();
    }

    pub fn shrink_to(&mut self, aligned_size: u64) {
        if aligned_size == 0 {
            self.remove_extents();
            return;
        }
        let mut sectors_to_remove = (self.size - aligned_size) / LP_SECTOR_SIZE;
        while sectors_to_remove > 0 {
            let Some(last) = self.extents.last_mut() else {
                break;
            };
            let len = last.num_sectors();
            if len > sectors_to_remove {
                self.size -= sectors_to_remove * LP_SECTOR_SIZE;
                last.set_num_sectors(len - sectors_to_remove);
                break;
            }
            self.size -= len * LP_SECTOR_SIZE;
            sectors_to_remove -= len;
            self.extents.pop();
        }
    }
}

#[derive(Debug, Clone)]
pub struct PartitionGroup {
    pub name: String,
    pub maximum_size: u64,
}

/// High-level builder for creating or modifying `LpMetadata` (matching AOSP `MetadataBuilder`).
#[derive(Debug, Clone)]
pub struct MetadataBuilder {
    geometry: LpMetadataGeometry,
    minor_version: u16,
    header_flags: u32,
    block_devices: Vec<LpMetadataBlockDevice>,
    groups: Vec<PartitionGroup>,
    partitions: Vec<PartitionBuilder>,
    auto_slot_suffixing: bool,
}

impl MetadataBuilder {
    /// Creates a brand-new `MetadataBuilder` from block device descriptors.
    pub fn new(
        block_devices: &[BlockDeviceInfo],
        super_partition: &str,
        metadata_max_size: u32,
        metadata_slot_count: u32,
    ) -> Result<Self> {
        let min_hdr_size = std::mem::size_of::<LpMetadataHeaderRaw>() as u32;
        if metadata_max_size < min_hdr_size {
            return Err(LpError::InvalidArgument(format!(
                "metadata_max_size ({metadata_max_size}) must be >= {min_hdr_size}"
            )));
        }
        if metadata_slot_count == 0 {
            return Err(LpError::InvalidArgument(
                "metadata_slot_count must be > 0".into(),
            ));
        }
        if block_devices.is_empty() {
            return Err(LpError::InvalidArgument(
                "at least one block device must be specified".into(),
            ));
        }

        let metadata_max_size = align_to(metadata_max_size as u64, LP_SECTOR_SIZE)? as u32;

        let mut logical_block_size: u32 = 0;
        let mut out_devices = Vec::with_capacity(block_devices.len());

        for dev in block_devices {
            verify_device_properties(dev)?;
            if logical_block_size == 0 {
                logical_block_size = dev.logical_block_size;
            } else if logical_block_size != dev.logical_block_size {
                return Err(LpError::InvalidArgument(
                    "all block devices must share the same logical_block_size".into(),
                ));
            }
            if dev.partition_name.len() > 36 {
                return Err(LpError::InvalidArgument(format!(
                    "block device name '{}' exceeds 36 bytes",
                    dev.partition_name
                )));
            }

            let align = if dev.alignment != 0 {
                dev.alignment as u64
            } else {
                logical_block_size as u64
            };
            let free_area_start = align_to(LP_SECTOR_SIZE, align)?;
            let first_logical_sector = free_area_start / LP_SECTOR_SIZE;
            let min_size = first_logical_sector * LP_SECTOR_SIZE + logical_block_size as u64;
            if dev.size < min_size {
                return Err(LpError::OutOfSpace(format!(
                    "block device '{}' size ({}) too small (need >= {min_size})",
                    dev.partition_name, dev.size
                )));
            }

            let entry = LpMetadataBlockDevice {
                first_logical_sector,
                alignment: dev.alignment,
                alignment_offset: dev.alignment_offset,
                size: dev.size,
                partition_name: dev.partition_name.clone(),
                flags: 0,
            };
            if dev.partition_name == super_partition {
                out_devices.insert(0, entry);
            } else {
                out_devices.push(entry);
            }
        }

        if out_devices
            .first()
            .is_none_or(|d| d.partition_name != super_partition)
        {
            return Err(LpError::InvalidArgument(format!(
                "super partition '{super_partition}' was not found in block_devices"
            )));
        }

        let total_reserved = get_total_metadata_size(metadata_max_size, metadata_slot_count);
        {
            let super_dev = &mut out_devices[0];
            if super_dev.size < total_reserved {
                return Err(LpError::OutOfSpace(format!(
                    "super device size ({}) smaller than reserved metadata ({total_reserved})",
                    super_dev.size
                )));
            }
            let align = if super_dev.alignment != 0 {
                super_dev.alignment as u64
            } else {
                logical_block_size as u64
            };
            let free_area_start = align_to(total_reserved, align)?;
            super_dev.first_logical_sector = free_area_start / LP_SECTOR_SIZE;
            let min_disk_size =
                super_dev.first_logical_sector * LP_SECTOR_SIZE + logical_block_size as u64;
            if super_dev.size < min_disk_size {
                return Err(LpError::OutOfSpace(format!(
                    "super device must be at least {min_disk_size} bytes, got {}",
                    super_dev.size
                )));
            }
        }

        let geometry = LpMetadataGeometry {
            magic: crate::format::LP_METADATA_GEOMETRY_MAGIC,
            struct_size: std::mem::size_of::<crate::format::LpMetadataGeometryRaw>() as u32,
            checksum: [0u8; 32],
            metadata_max_size,
            metadata_slot_count,
            logical_block_size,
        };

        let mut builder = Self {
            geometry,
            minor_version: LP_METADATA_MINOR_VERSION_MIN,
            header_flags: 0,
            block_devices: out_devices,
            groups: Vec::new(),
            partitions: Vec::new(),
            auto_slot_suffixing: false,
        };
        builder.add_group(LP_DEFAULT_GROUP, 0)?;
        Ok(builder)
    }

    /// Reconstructs a `MetadataBuilder` from existing `LpMetadata` (e.g. for `lpadd`).
    pub fn from_metadata(metadata: &LpMetadata) -> Result<Self> {
        let mut builder = Self {
            geometry: metadata.geometry.clone(),
            minor_version: metadata.header.minor_version,
            header_flags: metadata.header.flags,
            block_devices: metadata.block_devices.clone(),
            groups: Vec::new(),
            partitions: Vec::new(),
            auto_slot_suffixing: false,
        };

        for g in &metadata.groups {
            builder.add_group(&g.name, g.maximum_size)?;
        }

        for p in &metadata.partitions {
            let group_name = metadata
                .groups
                .get(p.group_index as usize)
                .ok_or_else(|| LpError::InvalidMetadata("partition group index out of bounds".into()))?
                .name
                .clone();
            builder.add_partition(&p.name, &group_name, p.attributes)?;
            let part = builder
                .partitions
                .last_mut()
                .expect("partition just pushed");
            let start = p.first_extent_index as usize;
            let end = start + p.num_extents as usize;
            for ext in &metadata.extents[start..end] {
                if ext.target_type == LP_TARGET_TYPE_LINEAR {
                    part.add_extent(Extent::Linear {
                        num_sectors: ext.num_sectors,
                        device_index: ext.target_source,
                        physical_sector: ext.target_data,
                    });
                } else if ext.target_type == LP_TARGET_TYPE_ZERO {
                    part.add_extent(Extent::Zero {
                        num_sectors: ext.num_sectors,
                    });
                }
            }
        }

        Ok(builder)
    }

    pub fn set_auto_slot_suffixing(&mut self, enabled: bool) {
        self.auto_slot_suffixing = enabled;
    }

    pub fn set_virtual_ab_device_flag(&mut self, enabled: bool) {
        if enabled {
            self.require_expanded_header();
            self.header_flags |= LP_HEADER_FLAG_VIRTUAL_AB_DEVICE;
        } else {
            self.header_flags &= !LP_HEADER_FLAG_VIRTUAL_AB_DEVICE;
        }
    }

    pub fn set_overlays_active_flag(&mut self, enabled: bool) {
        if enabled {
            self.require_expanded_header();
            self.header_flags |= LP_HEADER_FLAG_OVERLAYS_ACTIVE;
        } else {
            self.header_flags &= !LP_HEADER_FLAG_OVERLAYS_ACTIVE;
        }
    }

    fn require_expanded_header(&mut self) {
        if self.minor_version < LP_METADATA_VERSION_FOR_EXPANDED_HEADER {
            self.minor_version = LP_METADATA_VERSION_FOR_EXPANDED_HEADER;
        }
    }

    pub fn add_group(&mut self, name: impl Into<String>, maximum_size: u64) -> Result<()> {
        let name = name.into();
        if self.groups.iter().any(|g| g.name == name) {
            return Err(LpError::InvalidArgument(format!(
                "group already exists: {name}"
            )));
        }
        self.groups.push(PartitionGroup { name, maximum_size });
        Ok(())
    }

    pub fn add_partition(
        &mut self,
        name: impl Into<String>,
        group_name: impl Into<String>,
        attributes: u32,
    ) -> Result<()> {
        let name = name.into();
        let group_name = group_name.into();
        if name.is_empty() {
            return Err(LpError::InvalidArgument(
                "partition name cannot be empty".into(),
            ));
        }
        if self.partitions.iter().any(|p| p.name == name) {
            return Err(LpError::InvalidArgument(format!(
                "duplicate partition: {name}"
            )));
        }
        if !self.groups.iter().any(|g| g.name == group_name) {
            return Err(LpError::NotFound(format!(
                "partition group not found: {group_name}"
            )));
        }
        self.partitions
            .push(PartitionBuilder::new(name, group_name, attributes));
        Ok(())
    }

    pub fn remove_partition(&mut self, name: &str) {
        self.partitions.retain(|p| p.name != name);
    }

    pub fn find_partition(&self, name: &str) -> Option<&PartitionBuilder> {
        self.partitions.iter().find(|p| p.name == name)
    }

    pub fn logical_block_size(&self) -> u32 {
        self.geometry.logical_block_size
    }

    pub fn block_devices(&self) -> &[LpMetadataBlockDevice] {
        &self.block_devices
    }

    fn total_size_of_group(&self, group_name: &str) -> u64 {
        self.partitions
            .iter()
            .filter(|p| p.group_name == group_name)
            .map(|p| p.bytes_on_disk())
            .sum()
    }

    fn validate_partition_size_change(
        &self,
        part_idx: usize,
        old_size: u64,
        new_size: u64,
    ) -> Result<()> {
        if new_size <= old_size {
            return Ok(());
        }
        let part = &self.partitions[part_idx];
        let group = self
            .groups
            .iter()
            .find(|g| g.name == part.group_name)
            .ok_or_else(|| LpError::NotFound(format!("unknown group '{}'", part.group_name)))?;

        if group.maximum_size > 0 {
            let space_needed = new_size - old_size;
            let group_size = self.total_size_of_group(&group.name);
            let remaining = group.maximum_size.saturating_sub(group_size);
            if remaining < space_needed {
                return Err(LpError::GroupOutOfSpace {
                    partition: part.name.clone(),
                    group: group.name.clone(),
                    requested: space_needed,
                    used: group_size,
                    maximum: group.maximum_size,
                    remaining,
                    deficit: space_needed - remaining,
                });
            }
        }
        Ok(())
    }

    fn align_sector(&self, dev: &LpMetadataBlockDevice, sector: u64) -> Result<u64> {
        let lba = sector
            .checked_mul(LP_SECTOR_SIZE)
            .ok_or_else(|| LpError::InvalidArgument("sector overflow".into()))?;
        let a1 = align_to(lba, dev.alignment as u64)?;
        let a2 = align_to(a1, LP_SECTOR_SIZE)?;
        Ok(a2 / LP_SECTOR_SIZE)
    }

    /// Computes free sector intervals across all block devices.
    pub fn get_free_regions(&self) -> Result<Vec<Interval>> {
        let mut device_extents: Vec<Vec<Interval>> = vec![Vec::new(); self.block_devices.len()];
        for part in &self.partitions {
            for ext in &part.extents {
                if let Extent::Linear {
                    num_sectors,
                    device_index,
                    physical_sector,
                } = *ext
                {
                    let idx = device_index as usize;
                    if idx >= device_extents.len() {
                        return Err(LpError::InvalidMetadata(
                            "extent device_index out of bounds".into(),
                        ));
                    }
                    device_extents[idx].push(Interval {
                        device_index,
                        start: physical_sector,
                        end: physical_sector + num_sectors,
                    });
                }
            }
        }

        let mut free_regions = Vec::new();
        for (i, extents) in device_extents.iter_mut().enumerate() {
            let dev = &self.block_devices[i];
            let first = dev.first_logical_sector;
            let last = dev.size / LP_SECTOR_SIZE;
            extents.push(Interval {
                device_index: i as u32,
                start: first,
                end: first,
            });
            extents.push(Interval {
                device_index: i as u32,
                start: last,
                end: last,
            });
            extents.sort_by_key(|x| (x.start, x.end));

            for pair in extents.windows(2) {
                let prev = pair[0];
                let curr = pair[1];
                let aligned = self.align_sector(dev, prev.end)?;
                if aligned < curr.start {
                    free_regions.push(Interval {
                        device_index: curr.device_index,
                        start: aligned,
                        end: curr.start,
                    });
                }
            }
        }
        Ok(free_regions)
    }

    fn extend_final_extent(
        &self,
        part_idx: usize,
        free_list: &[Interval],
        sectors_needed: u64,
    ) -> Result<Option<Extent>> {
        let part = &self.partitions[part_idx];
        let Some(Extent::Linear {
            num_sectors,
            device_index,
            physical_sector,
        }) = part.extents.last().copied()
        else {
            return Ok(None);
        };

        let end_sector = physical_sector + num_sectors;
        let dev = &self.block_devices[device_index as usize];
        let next_aligned = self.align_sector(dev, end_sector)?;
        if end_sector == next_aligned {
            return Ok(None);
        }

        let extra = (next_aligned - end_sector).min(sectors_needed);
        let cand_start = end_sector;
        let cand_end = end_sector + extra;

        let overlaps_free = free_list.iter().any(|r| {
            r.device_index == device_index && cand_start < r.end && r.start < cand_end
        });
        let overlaps_alloc = self.partitions.iter().any(|p| {
            p.extents.iter().any(|e| match *e {
                Extent::Linear {
                    num_sectors: n,
                    device_index: d,
                    physical_sector: ps,
                } => d == device_index && cand_start < ps + n && ps < cand_end,
                Extent::Zero { .. } => false,
            })
        });

        if overlaps_free || overlaps_alloc {
            return Ok(None);
        }

        Ok(Some(Extent::Linear {
            num_sectors: extra,
            device_index,
            physical_sector: cand_start,
        }))
    }

    /// Resizes `partition_name` to `requested_size` (aligned up to `logical_block_size`).
    pub fn resize_partition(&mut self, partition_name: &str, requested_size: u64) -> Result<()> {
        let part_idx = self
            .partitions
            .iter()
            .position(|p| p.name == partition_name)
            .ok_or_else(|| LpError::NotFound(format!("partition '{partition_name}' not found")))?;

        let aligned_size = align_to(requested_size, self.geometry.logical_block_size as u64)?;
        let old_size = self.partitions[part_idx].size();
        self.validate_partition_size_change(part_idx, old_size, aligned_size)?;

        if aligned_size > old_size {
            let space_needed = aligned_size - old_size;
            let mut sectors_needed = space_needed / LP_SECTOR_SIZE;
            let sectors_per_block = (self.geometry.logical_block_size as u64) / LP_SECTOR_SIZE;

            let free_regions = self.get_free_regions()?;
            let mut new_extents = Vec::new();

            if let Some(ext) = self.extend_final_extent(part_idx, &free_regions, sectors_needed)? {
                sectors_needed -= ext.num_sectors();
                new_extents.push(ext);
            }

            for mut region in free_regions {
                if sectors_needed == 0 {
                    break;
                }
                if region.length() % sectors_per_block != 0 {
                    let rounded = (region.length() / sectors_per_block) * sectors_per_block;
                    region.end = region.start + rounded;
                    if region.length() == 0 {
                        continue;
                    }
                }
                let sectors = sectors_needed.min(region.length());
                new_extents.push(Extent::Linear {
                    num_sectors: sectors,
                    device_index: region.device_index,
                    physical_sector: region.start,
                });
                sectors_needed -= sectors;
            }

            if sectors_needed > 0 {
                let allocatable = self.allocatable_space();
                let used = self.used_space();
                let free = allocatable.saturating_sub(used);
                let deficit = sectors_needed * LP_SECTOR_SIZE;
                return Err(LpError::SuperOutOfSpace {
                    partition: partition_name.to_string(),
                    requested: aligned_size,
                    allocatable,
                    used,
                    free,
                    deficit,
                });
            }

            let part = &mut self.partitions[part_idx];
            for ext in new_extents {
                part.add_extent(ext);
            }
        } else if aligned_size < old_size {
            self.partitions[part_idx].shrink_to(aligned_size);
        }

        Ok(())
    }

    pub fn allocatable_space(&self) -> u64 {
        self.block_devices
            .iter()
            .map(|d| d.size.saturating_sub(d.first_logical_sector * LP_SECTOR_SIZE))
            .sum()
    }

    pub fn used_space(&self) -> u64 {
        self.partitions.iter().map(|p| p.size()).sum()
    }

    fn validate_groups(&self) -> Result<()> {
        for g in &self.groups {
            if g.maximum_size == 0 {
                continue;
            }
            let used = self.total_size_of_group(&g.name);
            if used > g.maximum_size {
                return Err(LpError::GroupOutOfSpace {
                    partition: String::from("<group total>"),
                    group: g.name.clone(),
                    requested: used,
                    used,
                    maximum: g.maximum_size,
                    remaining: 0,
                    deficit: used - g.maximum_size,
                });
            }
        }
        Ok(())
    }

    /// Exports the validated `LpMetadata` ready for serialization and image creation.
    pub fn export(&self) -> Result<LpMetadata> {
        self.validate_groups()?;

        let mut minor_version = self.minor_version.clamp(
            LP_METADATA_MINOR_VERSION_MIN,
            LP_METADATA_MINOR_VERSION_MAX,
        );

        let mut block_devices = Vec::with_capacity(self.block_devices.len());
        for dev in &self.block_devices {
            let mut copy = dev.clone();
            if self.auto_slot_suffixing {
                copy.flags |= LP_BLOCK_DEVICE_SLOT_SUFFIXED;
            }
            block_devices.push(copy);
        }

        let mut group_indices = HashMap::new();
        let mut groups = Vec::with_capacity(self.groups.len());
        for g in &self.groups {
            if g.name.len() > 36 {
                return Err(LpError::InvalidArgument(format!(
                    "group name too long: '{}'",
                    g.name
                )));
            }
            let flags = if self.auto_slot_suffixing && g.name != LP_DEFAULT_GROUP {
                LP_GROUP_SLOT_SUFFIXED
            } else {
                0
            };
            group_indices.insert(g.name.clone(), groups.len() as u32);
            groups.push(LpMetadataPartitionGroup {
                name: g.name.clone(),
                flags,
                maximum_size: g.maximum_size,
            });
        }

        let mut partitions = Vec::with_capacity(self.partitions.len());
        let mut extents = Vec::new();
        let mut seen_names = BTreeSet::new();

        for part in &self.partitions {
            if part.name.len() > 36 {
                return Err(LpError::InvalidArgument(format!(
                    "partition name too long: '{}'",
                    part.name
                )));
            }
            if !seen_names.insert(part.name.clone()) {
                return Err(LpError::InvalidArgument(format!(
                    "duplicate partition name: '{}'",
                    part.name
                )));
            }
            if part.attributes & !LP_PARTITION_ATTRIBUTE_MASK != 0 {
                return Err(LpError::InvalidArgument(format!(
                    "partition '{}' has unsupported attributes 0x{:x}",
                    part.name, part.attributes
                )));
            }
            if part.attributes & LP_PARTITION_ATTRIBUTE_MASK_V1 != 0 {
                minor_version = minor_version.max(LP_METADATA_VERSION_FOR_UPDATED_ATTR);
            }

            let group_index = *group_indices.get(&part.group_name).ok_or_else(|| {
                LpError::NotFound(format!("unknown group '{}'", part.group_name))
            })?;

            let first_extent_index = extents.len() as u32;
            let num_extents = part.extents.len() as u32;
            let mut attributes = part.attributes;
            if self.auto_slot_suffixing {
                attributes |= LP_PARTITION_ATTR_SLOT_SUFFIXED;
            }

            for ext in &part.extents {
                match *ext {
                    Extent::Linear {
                        num_sectors,
                        device_index,
                        physical_sector,
                    } => {
                        if device_index as usize >= block_devices.len() {
                            return Err(LpError::InvalidMetadata(
                                "extent references unknown block device".into(),
                            ));
                        }
                        extents.push(LpMetadataExtent {
                            num_sectors,
                            target_type: LP_TARGET_TYPE_LINEAR,
                            target_data: physical_sector,
                            target_source: device_index,
                        });
                    }
                    Extent::Zero { num_sectors } => {
                        extents.push(LpMetadataExtent {
                            num_sectors,
                            target_type: LP_TARGET_TYPE_ZERO,
                            target_data: 0,
                            target_source: 0,
                        });
                    }
                }
            }

            partitions.push(LpMetadataPartition {
                name: part.name.clone(),
                attributes,
                first_extent_index,
                num_extents,
                group_index,
            });
        }

        let header_size = if minor_version >= LP_METADATA_VERSION_FOR_EXPANDED_HEADER {
            std::mem::size_of::<LpMetadataHeaderRaw>() as u32
        } else {
            std::mem::size_of::<LpMetadataHeaderV1_0Raw>() as u32
        };

        let header = LpMetadataHeader {
            magic: crate::format::LP_METADATA_HEADER_MAGIC,
            major_version: LP_METADATA_MAJOR_VERSION,
            minor_version,
            header_size,
            header_checksum: [0u8; 32],
            tables_size: 0,
            tables_checksum: [0u8; 32],
            partitions: LpMetadataTableDescriptor {
                offset: 0,
                num_entries: partitions.len() as u32,
                entry_size: std::mem::size_of::<crate::format::LpMetadataPartitionRaw>() as u32,
            },
            extents: LpMetadataTableDescriptor {
                offset: 0,
                num_entries: extents.len() as u32,
                entry_size: std::mem::size_of::<crate::format::LpMetadataExtentRaw>() as u32,
            },
            groups: LpMetadataTableDescriptor {
                offset: 0,
                num_entries: groups.len() as u32,
                entry_size: std::mem::size_of::<crate::format::LpMetadataPartitionGroupRaw>() as u32,
            },
            block_devices: LpMetadataTableDescriptor {
                offset: 0,
                num_entries: block_devices.len() as u32,
                entry_size: std::mem::size_of::<crate::format::LpMetadataBlockDeviceRaw>() as u32,
            },
            flags: self.header_flags,
        };

        Ok(LpMetadata {
            geometry: self.geometry.clone(),
            header,
            partitions,
            extents,
            groups,
            block_devices,
        })
    }
}

fn verify_device_properties(dev: &BlockDeviceInfo) -> Result<()> {
    if dev.logical_block_size == 0 || dev.logical_block_size as u64 % LP_SECTOR_SIZE != 0 {
        let suggested = align_to(dev.logical_block_size.max(1) as u64, LP_SECTOR_SIZE).unwrap_or(4096);
        return Err(LpError::AlignmentError {
            field: format!("block device '{}' logical_block_size", dev.partition_name),
            value: dev.logical_block_size as u64,
            alignment: LP_SECTOR_SIZE,
            suggested,
        });
    }
    if dev.size % dev.logical_block_size as u64 != 0 {
        let suggested = align_to(dev.size, dev.logical_block_size as u64).unwrap_or(dev.size);
        return Err(LpError::AlignmentError {
            field: format!("block device '{}' size", dev.partition_name),
            value: dev.size,
            alignment: dev.logical_block_size as u64,
            suggested,
        });
    }
    if dev.alignment_offset as u64 % LP_SECTOR_SIZE != 0 {
        let suggested = align_to(dev.alignment_offset as u64, LP_SECTOR_SIZE).unwrap_or(0);
        return Err(LpError::AlignmentError {
            field: format!("block device '{}' alignment_offset", dev.partition_name),
            value: dev.alignment_offset as u64,
            alignment: LP_SECTOR_SIZE,
            suggested,
        });
    }
    if dev.alignment as u64 % LP_SECTOR_SIZE != 0 {
        let suggested = align_to(dev.alignment as u64, LP_SECTOR_SIZE).unwrap_or(0);
        return Err(LpError::AlignmentError {
            field: format!("block device '{}' alignment", dev.partition_name),
            value: dev.alignment as u64,
            alignment: LP_SECTOR_SIZE,
            suggested,
        });
    }
    Ok(())
}
