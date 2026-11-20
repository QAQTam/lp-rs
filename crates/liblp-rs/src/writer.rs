use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lp_sparse::{SparseBuilder, SparseChunk, SparseReader, is_sparse_image, uniform_fill_u32};
use zerocopy::IntoBytes;
use zerocopy::byteorder::little_endian::{U16, U32, U64};

use crate::error::{LpError, Result};
use crate::format::{
    LP_METADATA_GEOMETRY_SIZE, LP_METADATA_VERSION_FOR_EXPANDED_HEADER,
    LP_PARTITION_RESERVED_BYTES, LP_SECTOR_SIZE, LP_TARGET_TYPE_LINEAR, LpMetadata,
    LpMetadataBlockDeviceRaw, LpMetadataExtent, LpMetadataExtentRaw, LpMetadataGeometry,
    LpMetadataGeometryRaw, LpMetadataHeaderRaw, LpMetadataPartitionGroupRaw,
    LpMetadataPartitionRaw, LpMetadataTableDescriptorRaw, encode_fixed_str,
};
use crate::reader::{
    read_backup_metadata, read_logical_partition_geometry, read_primary_metadata,
};
use crate::utility::{
    align_to, compute_partition_size, get_backup_geometry_offset, get_backup_metadata_offset,
    get_metadata_super_block_device, get_primary_geometry_offset, get_primary_metadata_offset,
    get_total_metadata_size, get_total_super_partition_size, sha256,
};

/// Serializes `LpMetadataGeometry` into a zero-padded `LP_METADATA_GEOMETRY_SIZE`-byte blob
/// with its SHA-256 checksum populated.
pub fn serialize_geometry(geometry: &LpMetadataGeometry) -> Vec<u8> {
    let mut raw = LpMetadataGeometryRaw {
        magic: U32::new(geometry.magic),
        struct_size: U32::new(std::mem::size_of::<LpMetadataGeometryRaw>() as u32),
        checksum: [0u8; 32],
        metadata_max_size: U32::new(geometry.metadata_max_size),
        metadata_slot_count: U32::new(geometry.metadata_slot_count),
        logical_block_size: U32::new(geometry.logical_block_size),
    };
    raw.checksum = sha256(raw.as_bytes());

    let mut blob = Vec::with_capacity(LP_METADATA_GEOMETRY_SIZE);
    blob.extend_from_slice(raw.as_bytes());
    blob.resize(LP_METADATA_GEOMETRY_SIZE, 0);
    blob
}

/// Serializes `LpMetadata` header and all 4 tables, computing `tables_checksum` and `header_checksum`.
pub fn serialize_metadata(metadata: &LpMetadata) -> Result<Vec<u8>> {
    let mut partitions_bytes = Vec::with_capacity(
        metadata.partitions.len() * std::mem::size_of::<LpMetadataPartitionRaw>(),
    );
    for p in &metadata.partitions {
        let name = encode_fixed_str::<36>(&p.name).map_err(LpError::InvalidArgument)?;
        let raw = LpMetadataPartitionRaw {
            name,
            attributes: U32::new(p.attributes),
            first_extent_index: U32::new(p.first_extent_index),
            num_extents: U32::new(p.num_extents),
            group_index: U32::new(p.group_index),
        };
        partitions_bytes.extend_from_slice(raw.as_bytes());
    }

    let mut extents_bytes =
        Vec::with_capacity(metadata.extents.len() * std::mem::size_of::<LpMetadataExtentRaw>());
    for e in &metadata.extents {
        let raw = LpMetadataExtentRaw {
            num_sectors: U64::new(e.num_sectors),
            target_type: U32::new(e.target_type),
            target_data: U64::new(e.target_data),
            target_source: U32::new(e.target_source),
        };
        extents_bytes.extend_from_slice(raw.as_bytes());
    }

    let mut groups_bytes = Vec::with_capacity(
        metadata.groups.len() * std::mem::size_of::<LpMetadataPartitionGroupRaw>(),
    );
    for g in &metadata.groups {
        let name = encode_fixed_str::<36>(&g.name).map_err(LpError::InvalidArgument)?;
        let raw = LpMetadataPartitionGroupRaw {
            name,
            flags: U32::new(g.flags),
            maximum_size: U64::new(g.maximum_size),
        };
        groups_bytes.extend_from_slice(raw.as_bytes());
    }

    let mut block_devices_bytes = Vec::with_capacity(
        metadata.block_devices.len() * std::mem::size_of::<LpMetadataBlockDeviceRaw>(),
    );
    for d in &metadata.block_devices {
        let partition_name =
            encode_fixed_str::<36>(&d.partition_name).map_err(LpError::InvalidArgument)?;
        let raw = LpMetadataBlockDeviceRaw {
            first_logical_sector: U64::new(d.first_logical_sector),
            alignment: U32::new(d.alignment),
            alignment_offset: U32::new(d.alignment_offset),
            size: U64::new(d.size),
            partition_name,
            flags: U32::new(d.flags),
        };
        block_devices_bytes.extend_from_slice(raw.as_bytes());
    }

    let partitions_off = 0u32;
    let extents_off = partitions_off + partitions_bytes.len() as u32;
    let groups_off = extents_off + extents_bytes.len() as u32;
    let block_devices_off = groups_off + groups_bytes.len() as u32;
    let tables_size = block_devices_off + block_devices_bytes.len() as u32;

    let mut tables = Vec::with_capacity(tables_size as usize);
    tables.extend_from_slice(&partitions_bytes);
    tables.extend_from_slice(&extents_bytes);
    tables.extend_from_slice(&groups_bytes);
    tables.extend_from_slice(&block_devices_bytes);

    let tables_checksum = sha256(&tables);

    let header_size = if metadata.header.minor_version >= LP_METADATA_VERSION_FOR_EXPANDED_HEADER {
        std::mem::size_of::<LpMetadataHeaderRaw>() as u32
    } else {
        metadata.header.header_size
    };

    let mut hdr = LpMetadataHeaderRaw {
        magic: U32::new(metadata.header.magic),
        major_version: U16::new(metadata.header.major_version),
        minor_version: U16::new(metadata.header.minor_version),
        header_size: U32::new(header_size),
        header_checksum: [0u8; 32],
        tables_size: U32::new(tables_size),
        tables_checksum,
        partitions: LpMetadataTableDescriptorRaw {
            offset: U32::new(partitions_off),
            num_entries: U32::new(metadata.partitions.len() as u32),
            entry_size: U32::new(std::mem::size_of::<LpMetadataPartitionRaw>() as u32),
        },
        extents: LpMetadataTableDescriptorRaw {
            offset: U32::new(extents_off),
            num_entries: U32::new(metadata.extents.len() as u32),
            entry_size: U32::new(std::mem::size_of::<LpMetadataExtentRaw>() as u32),
        },
        groups: LpMetadataTableDescriptorRaw {
            offset: U32::new(groups_off),
            num_entries: U32::new(metadata.groups.len() as u32),
            entry_size: U32::new(std::mem::size_of::<LpMetadataPartitionGroupRaw>() as u32),
        },
        block_devices: LpMetadataTableDescriptorRaw {
            offset: U32::new(block_devices_off),
            num_entries: U32::new(metadata.block_devices.len() as u32),
            entry_size: U32::new(std::mem::size_of::<LpMetadataBlockDeviceRaw>() as u32),
        },
        flags: U32::new(metadata.header.flags),
        reserved: [0u8; 124],
    };

    let hdr_bytes_len = header_size as usize;
    let hdr_hash = sha256(&hdr.as_bytes()[..hdr_bytes_len]);
    hdr.header_checksum = hdr_hash;

    let mut out = Vec::with_capacity(hdr_bytes_len + tables.len());
    out.extend_from_slice(&hdr.as_bytes()[..hdr_bytes_len]);
    out.extend_from_slice(&tables);
    Ok(out)
}

/// Writes a minimal `super_empty.img` file (4KB geometry + 1 metadata copy).
pub fn write_empty_image_file(path: &Path, metadata: &LpMetadata) -> Result<()> {
    let geometry_blob = serialize_geometry(&metadata.geometry);
    let metadata_blob = serialize_metadata(metadata)?;
    if metadata_blob.len() > metadata.geometry.metadata_max_size as usize {
        let suggested = align_to(metadata_blob.len() as u64, LP_SECTOR_SIZE)? as u32;
        return Err(LpError::MetadataTooLarge {
            serialized_size: metadata_blob.len(),
            metadata_max_size: metadata.geometry.metadata_max_size,
            suggested_max_size: suggested,
        });
    }
    let mut file = BufWriter::new(File::create(path)?);
    file.write_all(&geometry_blob)?;
    file.write_all(&metadata_blob)?;
    file.flush()?;
    Ok(())
}

/// Writes a full single `super.img` (either Android sparse or raw) containing metadata and partition images.
pub fn write_to_image_file(
    path: &Path,
    metadata: &LpMetadata,
    block_size: u32,
    images: &HashMap<String, PathBuf>,
    sparsify: bool,
) -> Result<()> {
    let mut builder = ImageBuilder::new(metadata, block_size, images, sparsify)?;
    builder.build()?;
    builder.export(path)
}

/// Writes split `super_<device>.img` files into `output_dir` for multi-device retrofit setups.
pub fn write_split_image_files(
    output_dir: &Path,
    metadata: &LpMetadata,
    block_size: u32,
    images: &HashMap<String, PathBuf>,
    sparsify: bool,
) -> Result<()> {
    fs::create_dir_all(output_dir)?;
    let mut builder = ImageBuilder::new(metadata, block_size, images, sparsify)?;
    builder.build()?;
    builder.export_files(output_dir)
}

/// Builds complete sparse or raw `super` partition images from `LpMetadata` + partition `.img` files.
pub struct ImageBuilder<'a> {
    metadata: &'a LpMetadata,
    block_size: u32,
    sparsify: bool,
    images: BTreeMap<String, PathBuf>,
    device_images: Vec<SparseBuilder>,
}

impl<'a> ImageBuilder<'a> {
    pub fn new(
        metadata: &'a LpMetadata,
        block_size: u32,
        images: &HashMap<String, PathBuf>,
        sparsify: bool,
    ) -> Result<Self> {
        let total_size = get_total_super_partition_size(metadata);
        if block_size == 0 || block_size as u64 % LP_SECTOR_SIZE != 0 {
            return Err(LpError::InvalidArgument(format!(
                "block_size ({block_size}) must be a multiple of {LP_SECTOR_SIZE}"
            )));
        }
        if total_size % block_size as u64 != 0 {
            return Err(LpError::InvalidArgument(format!(
                "total super size ({total_size}) must be a multiple of block_size ({block_size})"
            )));
        }
        if metadata.geometry.metadata_max_size % block_size != 0 {
            return Err(LpError::InvalidArgument(format!(
                "metadata_max_size ({}) must be a multiple of block_size ({block_size})",
                metadata.geometry.metadata_max_size
            )));
        }
        if LP_METADATA_GEOMETRY_SIZE as u32 % block_size != 0 {
            return Err(LpError::InvalidArgument(format!(
                "geometry size ({LP_METADATA_GEOMETRY_SIZE}) not aligned to block_size ({block_size})"
            )));
        }
        if LP_PARTITION_RESERVED_BYTES % block_size as u64 != 0 {
            return Err(LpError::InvalidArgument(format!(
                "reserved bytes ({LP_PARTITION_RESERVED_BYTES}) not aligned to block_size ({block_size})"
            )));
        }

        let mut device_images = Vec::with_capacity(metadata.block_devices.len());
        for dev in &metadata.block_devices {
            device_images.push(SparseBuilder::new(block_size, dev.size)?);
        }

        let images = images
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Ok(Self {
            metadata,
            block_size,
            sparsify,
            images,
            device_images,
        })
    }

    fn sector_to_block(&self, sector: u64) -> Result<u64> {
        let bytes = sector * LP_SECTOR_SIZE;
        if bytes % self.block_size as u64 != 0 {
            return Err(LpError::InvalidMetadata(format!(
                "sector {sector} is not aligned to block_size {}",
                self.block_size
            )));
        }
        Ok(bytes / self.block_size as u64)
    }

    fn check_extent_ordering(&self) -> Result<()> {
        let mut last_sectors = vec![0u64; self.metadata.block_devices.len()];
        for ext in &self.metadata.extents {
            if ext.target_type != LP_TARGET_TYPE_LINEAR {
                return Err(LpError::InvalidMetadata(
                    "all extents must be LINEAR when building images".into(),
                ));
            }
            let dev_idx = ext.target_source as usize;
            if dev_idx >= last_sectors.len() {
                return Err(LpError::InvalidMetadata(
                    "extent references invalid block device".into(),
                ));
            }
            if ext.target_data <= last_sectors[dev_idx] && last_sectors[dev_idx] > 0 {
                return Err(LpError::InvalidMetadata(
                    "extents must appear in strictly increasing sector order".into(),
                ));
            }
            if (ext.num_sectors * LP_SECTOR_SIZE) % self.block_size as u64 != 0 {
                return Err(LpError::InvalidMetadata(
                    "extent size must be aligned to block_size".into(),
                ));
            }
            last_sectors[dev_idx] = ext.target_data;
        }
        Ok(())
    }

    pub fn build(&mut self) -> Result<()> {
        // Reserve initial 4KB zeroes at block 0.
        self.device_images[0].add_fill(0, 0, LP_PARTITION_RESERVED_BYTES)?;

        let geometry_blob = serialize_geometry(&self.metadata.geometry);
        let mut metadata_blob = serialize_metadata(self.metadata)?;
        if metadata_blob.len() > self.metadata.geometry.metadata_max_size as usize {
            let suggested = align_to(metadata_blob.len() as u64, LP_SECTOR_SIZE)? as u32;
            return Err(LpError::MetadataTooLarge {
                serialized_size: metadata_blob.len(),
                metadata_max_size: self.metadata.geometry.metadata_max_size,
                suggested_max_size: suggested,
            });
        }
        metadata_blob.resize(self.metadata.geometry.metadata_max_size as usize, 0);

        let copies = (self.metadata.geometry.metadata_slot_count as usize) * 2;
        let mut all_metadata =
            Vec::with_capacity(geometry_blob.len() * 2 + metadata_blob.len() * copies);
        all_metadata.extend_from_slice(&geometry_blob);
        all_metadata.extend_from_slice(&geometry_blob);
        for _ in 0..copies {
            all_metadata.extend_from_slice(&metadata_blob);
        }

        let first_block = self.sector_to_block(LP_PARTITION_RESERVED_BYTES / LP_SECTOR_SIZE)?;
        self.device_images[0].add_data(first_block, all_metadata)?;

        self.check_extent_ordering()?;

        let mut remaining_images = std::mem::take(&mut self.images);
        for part in &self.metadata.partitions {
            if let Some(img_path) = remaining_images.remove(&part.name) {
                self.add_partition_image(part, &img_path)?;
            }
        }

        if let Some((extra, _)) = remaining_images.into_iter().next() {
            return Err(LpError::NotFound(format!(
                "partition image specified for unknown partition '{extra}'"
            )));
        }

        Ok(())
    }

    fn add_partition_image(
        &mut self,
        part: &crate::format::LpMetadataPartition,
        img_path: &Path,
    ) -> Result<()> {
        if part.num_extents == 0 {
            return Err(LpError::InvalidArgument(format!(
                "partition '{}' has zero extents",
                part.name
            )));
        }

        let ext_start = part.first_extent_index as usize;
        let ext_end = ext_start + part.num_extents as usize;
        let extents: Vec<LpMetadataExtent> = self.metadata.extents[ext_start..ext_end].to_vec();
        let partition_size = compute_partition_size(self.metadata, part);

        let file = File::open(img_path)?;
        let mut reader = BufReader::new(file);
        let sparse_mode = is_sparse_image(&mut reader)?;

        if sparse_mode {
            let mut sparse = SparseReader::open(reader)?;
            let logical_len = sparse.total_size();
            if logical_len > partition_size {
                return Err(LpError::ImageExceedsPartition {
                    image_path: img_path.display().to_string(),
                    image_size: logical_len,
                    partition: part.name.clone(),
                    partition_size,
                    overflow: logical_len - partition_size,
                });
            }
            if sparse.block_size() == self.block_size {
                // Fast zero-copy-to-disk path: map sparse chunks directly across extents!
                let chunks = sparse.chunks().to_vec();
                let mut mapper = ExtentMapper::new(&extents, self.block_size);
                for chunk in chunks {
                    match chunk {
                        SparseChunk::Raw {
                            byte_len,
                            data_offset,
                            ..
                        } => {
                            let mut rem = byte_len;
                            let mut src_off = data_offset;
                            while rem > 0 {
                                let (dev_idx, out_block, take) = mapper.next_span(rem)?;
                                self.device_images[dev_idx].add_file_slice(
                                    out_block,
                                    img_path.to_path_buf(),
                                    src_off,
                                    take,
                                )?;
                                src_off += take;
                                rem -= take;
                            }
                        }
                        SparseChunk::Fill {
                            byte_len,
                            fill_bytes,
                            ..
                        } => {
                            let fill_val = u32::from_le_bytes(fill_bytes);
                            let mut rem = byte_len;
                            while rem > 0 {
                                let (dev_idx, out_block, take) = mapper.next_span(rem)?;
                                self.device_images[dev_idx].add_fill(out_block, fill_val, take)?;
                                rem -= take;
                            }
                        }
                        SparseChunk::DontCare { byte_len, .. } => {
                            let mut rem = byte_len;
                            while rem > 0 {
                                let (dev_idx, out_block, take) = mapper.next_span(rem)?;
                                self.device_images[dev_idx].add_fill(out_block, 0, take)?;
                                rem -= take;
                            }
                        }
                    }
                }
            } else {
                self.stream_into_extents(&mut sparse, logical_len, None, &extents, &part.name, img_path)?;
            }
        } else {
            let file_len = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(0))?;
            if file_len > partition_size {
                return Err(LpError::ImageExceedsPartition {
                    image_path: img_path.display().to_string(),
                    image_size: file_len,
                    partition: part.name.clone(),
                    partition_size,
                    overflow: file_len - partition_size,
                });
            }
            self.stream_into_extents(
                &mut reader,
                file_len,
                Some(img_path),
                &extents,
                &part.name,
                img_path,
            )?;
        }

        Ok(())
    }

    fn stream_into_extents<R: Read>(
        &mut self,
        reader: &mut R,
        total_len: u64,
        raw_file_path: Option<&Path>,
        extents: &[LpMetadataExtent],
        part_name: &str,
        img_path: &Path,
    ) -> Result<()> {
        if total_len == 0 {
            return Ok(());
        }
        let bs = self.block_size as usize;
        let mut mapper = ExtentMapper::new(extents, self.block_size);
        let mut buf = vec![0u8; bs];
        let mut pos = 0u64;

        // Track contiguous raw runs so we reference the file directly without storing in RAM.
        let mut pending_raw_dev: Option<usize> = None;
        let mut pending_raw_out_block: u64 = 0;
        let mut pending_raw_src_offset: u64 = 0;
        let mut pending_raw_bytes: u64 = 0;
        let mut pending_mem_buf: Vec<u8> = Vec::new();

        while pos < total_len {
            let to_read = ((total_len - pos) as usize).min(bs);
            reader.read_exact(&mut buf[..to_read])?;
            if to_read < bs {
                buf[to_read..].fill(0);
            }

            let (dev_idx, out_block, _span) = mapper.next_span(bs as u64).map_err(|_| {
                LpError::OutOfSpace(format!(
                    "image '{}' overflows extents of partition '{part_name}'",
                    img_path.display()
                ))
            })?;

            let fill_val = if to_read == bs {
                uniform_fill_u32(&buf)
            } else {
                None
            };

            if let Some(val) = fill_val {
                // Flush pending raw run first.
                flush_pending_raw(
                    &mut self.device_images,
                    raw_file_path,
                    &mut pending_raw_dev,
                    &mut pending_raw_out_block,
                    &mut pending_raw_src_offset,
                    &mut pending_raw_bytes,
                    &mut pending_mem_buf,
                )?;
                self.device_images[dev_idx].add_fill(out_block, val, bs as u64)?;
            } else if raw_file_path.is_some() && to_read == bs {
                let contiguous = pending_raw_dev == Some(dev_idx)
                    && pending_raw_out_block + (pending_raw_bytes / bs as u64) == out_block
                    && pending_raw_src_offset + pending_raw_bytes == pos;
                if contiguous {
                    pending_raw_bytes += bs as u64;
                } else {
                    flush_pending_raw(
                        &mut self.device_images,
                        raw_file_path,
                        &mut pending_raw_dev,
                        &mut pending_raw_out_block,
                        &mut pending_raw_src_offset,
                        &mut pending_raw_bytes,
                        &mut pending_mem_buf,
                    )?;
                    pending_raw_dev = Some(dev_idx);
                    pending_raw_out_block = out_block;
                    pending_raw_src_offset = pos;
                    pending_raw_bytes = bs as u64;
                }
            } else {
                let contiguous = pending_raw_dev == Some(dev_idx)
                    && pending_raw_out_block + (pending_mem_buf.len() as u64 / bs as u64)
                        == out_block;
                if !contiguous {
                    flush_pending_raw(
                        &mut self.device_images,
                        raw_file_path,
                        &mut pending_raw_dev,
                        &mut pending_raw_out_block,
                        &mut pending_raw_src_offset,
                        &mut pending_raw_bytes,
                        &mut pending_mem_buf,
                    )?;
                    pending_raw_dev = Some(dev_idx);
                    pending_raw_out_block = out_block;
                }
                pending_mem_buf.extend_from_slice(&buf);
                if pending_mem_buf.len() >= 16 * 1024 * 1024 {
                    flush_pending_raw(
                        &mut self.device_images,
                        raw_file_path,
                        &mut pending_raw_dev,
                        &mut pending_raw_out_block,
                        &mut pending_raw_src_offset,
                        &mut pending_raw_bytes,
                        &mut pending_mem_buf,
                    )?;
                }
            }

            pos += to_read as u64;
        }

        flush_pending_raw(
            &mut self.device_images,
            raw_file_path,
            &mut pending_raw_dev,
            &mut pending_raw_out_block,
            &mut pending_raw_src_offset,
            &mut pending_raw_bytes,
            &mut pending_mem_buf,
        )?;
        Ok(())
    }

    pub fn export(&self, path: &Path) -> Result<()> {
        if self.device_images.len() > 1 {
            return Err(LpError::InvalidArgument(
                "cannot export multi-device metadata to a single file; use write_split_image_files".into(),
            ));
        }
        let mut out = BufWriter::new(File::create(path)?);
        if self.sparsify {
            self.device_images[0].write_sparse(&mut out, false)?;
        } else {
            self.device_images[0].write_raw(&mut out)?;
        }
        Ok(())
    }

    pub fn export_files(&self, output_dir: &Path) -> Result<()> {
        for (i, sb) in self.device_images.iter().enumerate() {
            let dev_name = &self.metadata.block_devices[i].partition_name;
            let file_path = output_dir.join(format!("super_{dev_name}.img"));
            let mut out = BufWriter::new(File::create(file_path)?);
            if self.sparsify {
                sb.write_sparse(&mut out, false)?;
            } else {
                sb.write_raw(&mut out)?;
            }
        }
        Ok(())
    }
}

fn flush_pending_raw(
    device_images: &mut [SparseBuilder],
    raw_file_path: Option<&Path>,
    pending_dev: &mut Option<usize>,
    pending_out_block: &mut u64,
    pending_src_offset: &mut u64,
    pending_bytes: &mut u64,
    pending_mem_buf: &mut Vec<u8>,
) -> Result<()> {
    if let Some(dev_idx) = pending_dev.take() {
        if *pending_bytes > 0
            && let Some(path) = raw_file_path
        {
            device_images[dev_idx].add_file_slice(
                *pending_out_block,
                path.to_path_buf(),
                *pending_src_offset,
                *pending_bytes,
            )?;
        } else if !pending_mem_buf.is_empty() {
            let data = std::mem::take(pending_mem_buf);
            device_images[dev_idx].add_data(*pending_out_block, data)?;
        }
    }
    *pending_bytes = 0;
    Ok(())
}

struct ExtentMapper<'a> {
    extents: &'a [LpMetadataExtent],
    block_size: u32,
    idx: usize,
    offset_in_extent: u64,
}

impl<'a> ExtentMapper<'a> {
    fn new(extents: &'a [LpMetadataExtent], block_size: u32) -> Self {
        Self {
            extents,
            block_size,
            idx: 0,
            offset_in_extent: 0,
        }
    }

    fn next_span(&mut self, max_bytes: u64) -> Result<(usize, u64, u64)> {
        while self.idx < self.extents.len() {
            let ext = &self.extents[self.idx];
            let ext_bytes = ext.num_sectors * LP_SECTOR_SIZE;
            if self.offset_in_extent >= ext_bytes {
                self.idx += 1;
                self.offset_in_extent = 0;
                continue;
            }
            let avail = ext_bytes - self.offset_in_extent;
            let take = max_bytes.min(avail);
            let byte_pos = ext.target_data * LP_SECTOR_SIZE + self.offset_in_extent;
            if byte_pos % self.block_size as u64 != 0 {
                return Err(LpError::InvalidMetadata(
                    "extent byte offset not aligned to block_size".into(),
                ));
            }
            let out_block = byte_pos / self.block_size as u64;
            self.offset_in_extent += take;
            return Ok((ext.target_source as usize, out_block, take));
        }
        Err(LpError::OutOfSpace("ran out of partition extents".into()))
    }
}

/// Validates `metadata` bounds and serializes it into a metadata blob.
pub fn validate_and_serialize_metadata(metadata: &LpMetadata) -> Result<Vec<u8>> {
    let geometry = &metadata.geometry;
    let blob = serialize_metadata(metadata)?;
    if blob.len() > geometry.metadata_max_size as usize {
        let suggested = align_to(blob.len() as u64, LP_SECTOR_SIZE)? as u32;
        return Err(LpError::MetadataTooLarge {
            serialized_size: blob.len(),
            metadata_max_size: geometry.metadata_max_size,
            suggested_max_size: suggested,
        });
    }

    let total_reserved =
        get_total_metadata_size(geometry.metadata_max_size, geometry.metadata_slot_count);
    let super_dev = get_metadata_super_block_device(metadata)
        .ok_or_else(|| LpError::InvalidMetadata("metadata has no super block device".into()))?;
    if total_reserved > super_dev.first_logical_sector * LP_SECTOR_SIZE {
        return Err(LpError::OutOfSpace(
            "not enough space to store all logical partition metadata slots".into(),
        ));
    }

    for dev in &metadata.block_devices {
        if (dev.first_logical_sector + 1) * LP_SECTOR_SIZE > dev.size {
            return Err(LpError::InvalidMetadata(format!(
                "block device '{}' has invalid first_logical_sector {} for size {}",
                dev.partition_name, dev.first_logical_sector, dev.size
            )));
        }
    }

    for part in &metadata.partitions {
        let end = part
            .first_extent_index
            .checked_add(part.num_extents)
            .ok_or_else(|| LpError::InvalidMetadata("partition extent overflow".into()))?;
        if end as usize > metadata.extents.len() {
            return Err(LpError::InvalidMetadata(format!(
                "partition '{}' references out-of-bounds extents",
                part.name
            )));
        }
    }

    for ext in &metadata.extents {
        if ext.target_type == LP_TARGET_TYPE_LINEAR {
            let dev = metadata
                .block_devices
                .get(ext.target_source as usize)
                .ok_or_else(|| {
                    LpError::InvalidMetadata("extent references invalid block device".into())
                })?;
            let last_sector = dev.size / LP_SECTOR_SIZE;
            if ext.target_data < dev.first_logical_sector
                || ext.target_data + ext.num_sectors > last_sector
            {
                return Err(LpError::InvalidMetadata(
                    "extent range is out of block device bounds".into(),
                ));
            }
        }
    }

    Ok(blob)
}

fn validate_metadata_region(metadata: &LpMetadata, start: u64, size: usize) -> Result<()> {
    let super_dev = get_metadata_super_block_device(metadata)
        .ok_or_else(|| LpError::InvalidMetadata("metadata has no super block device".into()))?;
    if start + size as u64 >= super_dev.first_logical_sector * LP_SECTOR_SIZE {
        return Err(LpError::OutOfSpace(format!(
            "metadata write at {start}..{} overlaps logical partition contents",
            start + size as u64
        )));
    }
    Ok(())
}

fn write_metadata_slot(
    file: &mut File,
    metadata: &LpMetadata,
    slot_number: u32,
    blob: &[u8],
) -> Result<()> {
    if slot_number >= metadata.geometry.metadata_slot_count {
        return Err(LpError::InvalidArgument(format!(
            "invalid metadata slot {slot_number}"
        )));
    }
    let primary_off = get_primary_metadata_offset(&metadata.geometry, slot_number);
    validate_metadata_region(metadata, primary_off, blob.len())?;
    file.seek(SeekFrom::Start(primary_off))?;
    file.write_all(blob)?;

    let backup_off = get_backup_metadata_offset(&metadata.geometry, slot_number);
    validate_metadata_region(metadata, backup_off, blob.len())?;
    file.seek(SeekFrom::Start(backup_off))?;
    file.write_all(blob)?;
    Ok(())
}

/// Flashes geometry and partition tables for all slots onto a block device or raw `super.img`
/// (equivalent to AOSP `FlashPartitionTable` / `lpflash`).
pub fn flash_partition_table(target_path: &Path, metadata: &LpMetadata) -> Result<()> {
    let metadata_blob = validate_and_serialize_metadata(metadata)?;
    let mut file = OpenOptions::new().read(true).write(true).open(target_path)?;

    let zeroes = [0u8; LP_PARTITION_RESERVED_BYTES as usize];
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&zeroes)?;

    let geometry_blob = serialize_geometry(&metadata.geometry);
    file.seek(SeekFrom::Start(get_primary_geometry_offset()))?;
    file.write_all(&geometry_blob)?;
    file.seek(SeekFrom::Start(get_backup_geometry_offset()))?;
    file.write_all(&geometry_blob)?;

    for slot in 0..metadata.geometry.metadata_slot_count {
        write_metadata_slot(&mut file, metadata, slot, &metadata_blob)?;
    }

    file.flush()?;
    let _ = file.sync_all();
    Ok(())
}

/// Updates partition table metadata on `slot_number` of an existing block device or raw `super.img`
/// (equivalent to AOSP `UpdatePartitionTable`).
pub fn update_partition_table(
    target_path: &Path,
    metadata: &LpMetadata,
    slot_number: u32,
) -> Result<()> {
    let blob = validate_and_serialize_metadata(metadata)?;
    let mut file = OpenOptions::new().read(true).write(true).open(target_path)?;

    let old_geometry = read_logical_partition_geometry(&mut file)?;
    let g = &metadata.geometry;
    if g.metadata_max_size != old_geometry.metadata_max_size
        || g.metadata_slot_count != old_geometry.metadata_slot_count
        || g.logical_block_size != old_geometry.logical_block_size
    {
        return Err(LpError::InvalidGeometry(
            "new metadata geometry is incompatible with existing super image".into(),
        ));
    }
    if slot_number >= g.metadata_slot_count {
        return Err(LpError::InvalidArgument(format!(
            "invalid metadata slot {slot_number}"
        )));
    }

    let primary = read_primary_metadata(&mut file, g, slot_number).ok();
    let backup = read_backup_metadata(&mut file, g, slot_number).ok();

    match (primary, backup) {
        (Some(p), None) => {
            let old_blob = validate_and_serialize_metadata(&p)?;
            let backup_off = get_backup_metadata_offset(g, slot_number);
            validate_metadata_region(metadata, backup_off, old_blob.len())?;
            file.seek(SeekFrom::Start(backup_off))?;
            file.write_all(&old_blob)?;
        }
        (Some(p), Some(b)) if p.header.header_checksum != b.header.header_checksum => {
            let old_blob = validate_and_serialize_metadata(&p)?;
            let backup_off = get_backup_metadata_offset(g, slot_number);
            validate_metadata_region(metadata, backup_off, old_blob.len())?;
            file.seek(SeekFrom::Start(backup_off))?;
            file.write_all(&old_blob)?;
        }
        (None, Some(b)) => {
            let old_blob = validate_and_serialize_metadata(&b)?;
            let primary_off = get_primary_metadata_offset(g, slot_number);
            validate_metadata_region(metadata, primary_off, old_blob.len())?;
            file.seek(SeekFrom::Start(primary_off))?;
            file.write_all(&old_blob)?;
        }
        _ => {}
    }

    write_metadata_slot(&mut file, metadata, slot_number, &blob)?;
    file.flush()?;
    Ok(())
}

/// Writes `image_path` (raw or sparse) into the allocated extents of `partition_name` inside a raw `super.img`.
pub fn write_partition_to_raw_super(
    super_path: &Path,
    metadata: &LpMetadata,
    partition_name: &str,
    image_path: &Path,
) -> Result<()> {
    let part = metadata
        .partitions
        .iter()
        .find(|p| p.name == partition_name)
        .ok_or_else(|| LpError::NotFound(format!("partition '{partition_name}' not found")))?;

    let part_size = compute_partition_size(metadata, part);
    let mut super_file = OpenOptions::new().read(true).write(true).open(super_path)?;

    let src_file = File::open(image_path)?;
    let mut src_reader = BufReader::new(src_file);
    let is_sparse = is_sparse_image(&mut src_reader)?;

    let (mut stream, total_len): (Box<dyn ReadSeek>, u64) = if is_sparse {
        let sparse = SparseReader::open(src_reader)?;
        let len = sparse.total_size();
        (Box::new(sparse), len)
    } else {
        let len = src_reader.seek(SeekFrom::End(0))?;
        src_reader.seek(SeekFrom::Start(0))?;
        (Box::new(src_reader), len)
    };

    if total_len > part_size {
        return Err(LpError::ImageExceedsPartition {
            image_path: image_path.display().to_string(),
            image_size: total_len,
            partition: partition_name.to_string(),
            partition_size: part_size,
            overflow: total_len - part_size,
        });
    }

    let mut remaining = total_len;
    let mut buf = vec![0u8; 1024 * 1024];
    let ext_start = part.first_extent_index as usize;
    let ext_end = ext_start + part.num_extents as usize;

    for ext in &metadata.extents[ext_start..ext_end] {
        if remaining == 0 {
            break;
        }
        if ext.target_type != LP_TARGET_TYPE_LINEAR || ext.target_source != 0 {
            return Err(LpError::InvalidMetadata(
                "lpadd only supports single-device linear extents".into(),
            ));
        }
        let ext_bytes = ext.num_sectors * LP_SECTOR_SIZE;
        let mut to_copy = remaining.min(ext_bytes);
        super_file.seek(SeekFrom::Start(ext.target_data * LP_SECTOR_SIZE))?;
        while to_copy > 0 {
            let n = (to_copy as usize).min(buf.len());
            stream.read_exact(&mut buf[..n])?;
            super_file.write_all(&buf[..n])?;
            to_copy -= n as u64;
            remaining -= n as u64;
        }
    }

    super_file.flush()?;
    Ok(())
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Updates a sparse `super.img` in place (via sibling temp file + rename) with `new_metadata`
/// and optional `new_partition_name` / `image_path` data, without unpacking the whole image to disk.
pub fn update_sparse_super_image(
    super_path: &Path,
    new_metadata: &LpMetadata,
    new_partition_name: Option<&str>,
    image_path: Option<&Path>,
) -> Result<()> {
    let metadata_blob = validate_and_serialize_metadata(new_metadata)?;
    let (block_size, _total_size, old_chunks) = {
        let sparse = SparseReader::open(BufReader::new(File::open(super_path)?))?;
        (
            sparse.block_size(),
            sparse.total_size(),
            sparse.chunks().to_vec(),
        )
    };

    if new_metadata.block_devices.len() != 1 {
        return Err(LpError::InvalidArgument(
            "lpadd on sparse super.img only supports single-device super images".into(),
        ));
    }

    let slots_to_update = new_metadata.geometry.metadata_slot_count.min(2);
    let meta_region_end_bytes = LP_PARTITION_RESERVED_BYTES
        + (LP_METADATA_GEOMETRY_SIZE as u64) * 2
        + (new_metadata.geometry.metadata_max_size as u64) * (slots_to_update as u64);
    let meta_end_block = meta_region_end_bytes.div_ceil(block_size as u64);

    // Collect block intervals that will be rewritten:
    // 1) primary metadata slots 0..slots_to_update + geometry + reserved 4KB
    // 2) backup metadata slots 0..slots_to_update
    // 3) newly added partition's extents (if image_path is specified)
    let mut overwritten: Vec<(u64, u64)> = vec![(0, meta_end_block)];
    let backup_start_bytes = get_backup_metadata_offset(&new_metadata.geometry, 0);
    let backup_end_bytes =
        backup_start_bytes + (new_metadata.geometry.metadata_max_size as u64) * (slots_to_update as u64);
    if backup_start_bytes % block_size as u64 != 0 || backup_end_bytes % block_size as u64 != 0 {
        return Err(LpError::InvalidMetadata(
            "backup metadata region is not aligned to sparse block_size".into(),
        ));
    }
    overwritten.push((
        backup_start_bytes / block_size as u64,
        backup_end_bytes / block_size as u64,
    ));

    if let (Some(part_name), Some(_)) = (new_partition_name, image_path) {
        let part = new_metadata
            .partitions
            .iter()
            .find(|p| p.name == part_name)
            .ok_or_else(|| LpError::NotFound(format!("partition '{part_name}' not found")))?;
        let s = part.first_extent_index as usize;
        let e = s + part.num_extents as usize;
        for ext in &new_metadata.extents[s..e] {
            let b_start = (ext.target_data * LP_SECTOR_SIZE) / block_size as u64;
            let b_end = ((ext.target_data + ext.num_sectors) * LP_SECTOR_SIZE) / block_size as u64;
            overwritten.push((b_start, b_end));
        }
    }
    overwritten.sort_unstable();

    let mut image_map = HashMap::new();
    if let (Some(part_name), Some(img)) = (new_partition_name, image_path) {
        image_map.insert(part_name.to_string(), img.to_path_buf());
    }
    let mut img_builder = ImageBuilder::new(new_metadata, block_size, &image_map, true)?;
    // Copy existing untouched chunks first, subtracting overwritten block intervals.
    for chunk in old_chunks {
        let c_start = chunk.output_offset() / block_size as u64;
        let c_end = c_start + chunk.byte_len() / block_size as u64;
        let kept = subtract_intervals(c_start, c_end, &overwritten);
        for (k_start, k_end) in kept {
            let k_bytes = (k_end - k_start) * block_size as u64;
            match chunk {
                SparseChunk::Raw { data_offset, .. } => {
                    let delta = (k_start - c_start) * block_size as u64;
                    img_builder.device_images[0].add_file_slice(
                        k_start,
                        super_path.to_path_buf(),
                        data_offset + delta,
                        k_bytes,
                    )?;
                }
                SparseChunk::Fill { fill_bytes, .. } => {
                    img_builder.device_images[0].add_fill(
                        k_start,
                        u32::from_le_bytes(fill_bytes),
                        k_bytes,
                    )?;
                }
                SparseChunk::DontCare { .. } => {}
            }
        }
    }

    // Write reserved zeroes + geometry + primary metadata slots 0..slots_to_update.
    img_builder.device_images[0].add_fill(0, 0, LP_PARTITION_RESERVED_BYTES)?;
    let geometry_blob = serialize_geometry(&new_metadata.geometry);
    let mut padded_meta = metadata_blob;
    padded_meta.resize(new_metadata.geometry.metadata_max_size as usize, 0);

    let mut primary_region =
        Vec::with_capacity(geometry_blob.len() * 2 + padded_meta.len() * slots_to_update as usize);
    primary_region.extend_from_slice(&geometry_blob);
    primary_region.extend_from_slice(&geometry_blob);
    for _ in 0..slots_to_update {
        primary_region.extend_from_slice(&padded_meta);
    }
    let first_block = img_builder.sector_to_block(LP_PARTITION_RESERVED_BYTES / LP_SECTOR_SIZE)?;
    img_builder.device_images[0].add_data(first_block, primary_region)?;

    // Write backup metadata slots 0..slots_to_update.
    let mut backup_region = Vec::with_capacity(padded_meta.len() * slots_to_update as usize);
    for _ in 0..slots_to_update {
        backup_region.extend_from_slice(&padded_meta);
    }
    img_builder.device_images[0].add_data(backup_start_bytes / block_size as u64, backup_region)?;

    // Stream the new partition image into its extents.
    if let (Some(part_name), Some(img)) = (new_partition_name, image_path) {
        let part = new_metadata
            .partitions
            .iter()
            .find(|p| p.name == part_name)
            .unwrap();
        img_builder.add_partition_image(part, img)?;
    }

    let tmp_path = super_path.with_extension("lpadd.tmp");
    {
        let mut out = BufWriter::new(File::create(&tmp_path)?);
        img_builder.device_images[0].write_sparse(&mut out, false)?;
    }
    // Replace original sparse super.img.
    fs::rename(&tmp_path, super_path)?;
    Ok(())
}

fn subtract_intervals(mut start: u64, end: u64, cut: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for &(cs, ce) in cut {
        if ce <= start {
            continue;
        }
        if cs >= end {
            break;
        }
        if cs > start {
            out.push((start, cs));
        }
        start = start.max(ce);
        if start >= end {
            return out;
        }
    }
    if start < end {
        out.push((start, end));
    }
    out
}
