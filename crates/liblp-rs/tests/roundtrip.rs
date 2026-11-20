use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;

use liblp::{
    BlockDeviceInfo, LP_HEADER_FLAG_VIRTUAL_AB_DEVICE, LP_PARTITION_ATTR_READONLY,
    LP_PARTITION_ATTR_SLOT_SUFFIXED, LpError, MetadataBuilder, compute_partition_size,
    flash_partition_table, read_from_image_file, read_metadata, read_metadata_from_reader,
    update_partition_table, update_sparse_super_image, write_empty_image_file,
    write_partition_to_raw_super, write_to_image_file,
};
use lp_sparse::{SparseReader, encode_raw_to_sparse};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lp_rs_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn empty_super_image_and_builder_roundtrip() {
    let dir = temp_dir("empty_super");
    let out_path = dir.join("super_empty.img");

    let devices = vec![BlockDeviceInfo::new(
        "super",
        128 * 1024 * 1024,
        1024 * 1024,
        0,
        4096,
    )];
    let mut builder = MetadataBuilder::new(&devices, "super", 65536, 2).unwrap();
    builder.set_auto_slot_suffixing(true);
    builder.set_virtual_ab_device_flag(true);
    builder
        .add_group("qti_dynamic_partitions", 64 * 1024 * 1024)
        .unwrap();
    builder
        .add_partition(
            "system",
            "qti_dynamic_partitions",
            LP_PARTITION_ATTR_READONLY,
        )
        .unwrap();
    builder.resize_partition("system", 4 * 1024 * 1024).unwrap();

    let meta = builder.export().unwrap();
    assert_eq!(meta.header.minor_version, 2);
    assert_ne!(meta.header.flags & LP_HEADER_FLAG_VIRTUAL_AB_DEVICE, 0);
    assert_ne!(
        meta.partitions[0].attributes & LP_PARTITION_ATTR_SLOT_SUFFIXED,
        0
    );

    write_empty_image_file(&out_path, &meta).unwrap();

    let parsed_raw = read_from_image_file(&out_path).unwrap();
    assert_eq!(parsed_raw.partitions.len(), 1);
    assert_eq!(parsed_raw.partitions[0].name, "system");

    let parsed_slot_b = read_metadata(&out_path, 1).unwrap();
    assert_eq!(parsed_slot_b.partitions[0].name, "system_b");
    assert_eq!(parsed_slot_b.groups[1].name, "qti_dynamic_partitions_b");
    assert_eq!(parsed_slot_b.block_devices[0].partition_name, "super_b");
    assert_eq!(
        compute_partition_size(&parsed_slot_b, &parsed_slot_b.partitions[0]),
        4 * 1024 * 1024
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn full_super_make_and_read_sparse_and_raw() {
    let dir = temp_dir("full_super");

    // Create 1 raw partition image (system) and 1 sparse partition image (vendor).
    let sys_path = dir.join("system.img");
    let mut sys_bytes = vec![0u8; 2 * 1024 * 1024];
    for (i, b) in sys_bytes.iter_mut().enumerate() {
        *b = ((i * 7) % 256) as u8;
    }
    fs::write(&sys_path, &sys_bytes).unwrap();

    let vnd_raw = vec![0x5Au8; 1024 * 1024];
    let vnd_path = dir.join("vendor.sparse.img");
    let mut vnd_sparse = Vec::new();
    encode_raw_to_sparse(&mut Cursor::new(&vnd_raw), &mut vnd_sparse, 4096, true).unwrap();
    fs::write(&vnd_path, &vnd_sparse).unwrap();

    let devices = vec![BlockDeviceInfo::new(
        "super",
        32 * 1024 * 1024,
        1024 * 1024,
        0,
        4096,
    )];
    let mut builder = MetadataBuilder::new(&devices, "super", 65536, 2).unwrap();
    builder.add_group("main_a", 16 * 1024 * 1024).unwrap();
    builder
        .add_partition("system_a", "main_a", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    builder
        .resize_partition("system_a", sys_bytes.len() as u64)
        .unwrap();
    builder
        .add_partition("vendor_a", "main_a", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    builder
        .resize_partition("vendor_a", vnd_raw.len() as u64)
        .unwrap();

    let meta = builder.export().unwrap();
    let mut images = HashMap::new();
    images.insert("system_a".to_string(), sys_path);
    images.insert("vendor_a".to_string(), vnd_path);

    let super_sparse_path = dir.join("super_sparse.img");
    write_to_image_file(&super_sparse_path, &meta, 4096, &images, true).unwrap();

    // Verify we can read metadata directly from sparse super.img
    let read_back = read_metadata(&super_sparse_path, 0).unwrap();
    assert_eq!(read_back.partitions.len(), 2);
    assert_eq!(read_back.partitions[0].name, "system_a");
    assert_eq!(read_back.partitions[1].name, "vendor_a");

    // Verify partition payload bytes via SparseReader
    let mut sparse_reader = SparseReader::open(File::open(&super_sparse_path).unwrap()).unwrap();
    let sys_ext = &read_back.extents[read_back.partitions[0].first_extent_index as usize];
    sparse_reader
        .seek(SeekFrom::Start(sys_ext.target_data * 512))
        .unwrap();
    let mut got_sys = vec![0u8; sys_bytes.len()];
    sparse_reader.read_exact(&mut got_sys).unwrap();
    assert_eq!(got_sys, sys_bytes);

    let vnd_ext = &read_back.extents[read_back.partitions[1].first_extent_index as usize];
    sparse_reader
        .seek(SeekFrom::Start(vnd_ext.target_data * 512))
        .unwrap();
    let mut got_vnd = vec![0u8; vnd_raw.len()];
    sparse_reader.read_exact(&mut got_vnd).unwrap();
    assert_eq!(got_vnd, vnd_raw);

    // Also test group size overflow rejection with diagnostic fields
    let group_err = builder
        .resize_partition("vendor_a", 20 * 1024 * 1024)
        .unwrap_err();
    match group_err {
        LpError::GroupOutOfSpace {
            group,
            maximum,
            deficit,
            ..
        } => {
            assert_eq!(group, "main_a");
            assert_eq!(maximum, 16 * 1024 * 1024);
            assert!(deficit > 0);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Test SuperOutOfSpace diagnostics
    builder.add_group("extra", 0).unwrap();
    builder
        .add_partition("huge", "extra", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    let super_err = builder
        .resize_partition("huge", 64 * 1024 * 1024)
        .unwrap_err();
    assert!(matches!(super_err, LpError::SuperOutOfSpace { .. }));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn diagnostic_geometry_and_metadata_errors() {
    let blank = vec![0u8; 64 * 1024];
    let err = read_metadata_from_reader(&mut Cursor::new(blank), 0).unwrap_err();
    match &err {
        LpError::GeometryReadFailed { primary, backup } => {
            assert!(matches!(
                **primary,
                LpError::InvalidGeometryMagic {
                    offset: 4096,
                    found: 0,
                    ..
                }
            ));
            assert!(matches!(
                **backup,
                LpError::InvalidGeometryMagic {
                    offset: 8192,
                    found: 0,
                    ..
                }
            ));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("0x1000") && msg.contains("0x2000"));
}

#[test]
fn lpflash_and_lpadd_raw_and_sparse_roundtrip() {
    let dir = temp_dir("lpflash_lpadd");

    // 1) Build super_empty.img and flash it onto a raw super.img file (lpflash).
    let devices = vec![BlockDeviceInfo::new(
        "super",
        32 * 1024 * 1024,
        1024 * 1024,
        0,
        4096,
    )];
    let mut builder = MetadataBuilder::new(&devices, "super", 65536, 2).unwrap();
    builder.add_group("main", 16 * 1024 * 1024).unwrap();
    builder
        .add_partition("system", "main", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    builder.resize_partition("system", 1024 * 1024).unwrap();
    let meta = builder.export().unwrap();

    let empty_path = dir.join("super_empty.img");
    write_empty_image_file(&empty_path, &meta).unwrap();
    let parsed_empty = read_from_image_file(&empty_path).unwrap();

    let raw_super = dir.join("super_raw.img");
    {
        let f = File::create(&raw_super).unwrap();
        f.set_len(32 * 1024 * 1024).unwrap();
    }
    flash_partition_table(&raw_super, &parsed_empty).unwrap();

    // 2) Add a new partition 'odm' with data onto raw_super (lpadd raw path).
    let odm_bytes = vec![0x3Cu8; 512 * 1024];
    let odm_path = dir.join("odm.img");
    fs::write(&odm_path, &odm_bytes).unwrap();

    let mut b2 = MetadataBuilder::from_metadata(&read_metadata(&raw_super, 0).unwrap()).unwrap();
    b2.add_partition("odm", "main", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    b2.resize_partition("odm", odm_bytes.len() as u64).unwrap();
    let meta2 = b2.export().unwrap();
    update_partition_table(&raw_super, &meta2, 0).unwrap();
    write_partition_to_raw_super(&raw_super, &meta2, "odm", &odm_path).unwrap();

    let read_raw = read_metadata(&raw_super, 0).unwrap();
    assert_eq!(read_raw.partitions.len(), 2);
    assert_eq!(read_raw.partitions[1].name, "odm");

    // 3) Build sparse super.img and add 'product' via update_sparse_super_image (lpadd sparse path).
    let sparse_super = dir.join("super_sparse.img");
    let mut initial_imgs = HashMap::new();
    initial_imgs.insert("odm".to_string(), odm_path.clone());
    write_to_image_file(&sparse_super, &meta2, 4096, &initial_imgs, true).unwrap();

    let prod_bytes = vec![0x7Eu8; 256 * 1024];
    let prod_path = dir.join("product.img");
    fs::write(&prod_path, &prod_bytes).unwrap();

    let mut b3 = MetadataBuilder::from_metadata(&read_metadata(&sparse_super, 0).unwrap()).unwrap();
    b3.add_partition("product", "main", LP_PARTITION_ATTR_READONLY)
        .unwrap();
    b3.resize_partition("product", prod_bytes.len() as u64)
        .unwrap();
    let meta3 = b3.export().unwrap();
    update_sparse_super_image(&sparse_super, &meta3, Some("product"), Some(&prod_path)).unwrap();

    let read_sparse = read_metadata(&sparse_super, 0).unwrap();
    assert_eq!(read_sparse.partitions.len(), 3);
    assert_eq!(read_sparse.partitions[2].name, "product");

    let mut sr = SparseReader::open(File::open(&sparse_super).unwrap()).unwrap();
    let odm_ext = &read_sparse.extents[read_sparse.partitions[1].first_extent_index as usize];
    sr.seek(SeekFrom::Start(odm_ext.target_data * 512)).unwrap();
    let mut got_odm = vec![0u8; odm_bytes.len()];
    sr.read_exact(&mut got_odm).unwrap();
    assert_eq!(got_odm, odm_bytes);

    let prod_ext = &read_sparse.extents[read_sparse.partitions[2].first_extent_index as usize];
    sr.seek(SeekFrom::Start(prod_ext.target_data * 512))
        .unwrap();
    let mut got_prod = vec![0u8; prod_bytes.len()];
    sr.read_exact(&mut got_prod).unwrap();
    assert_eq!(got_prod, prod_bytes);

    let _ = fs::remove_dir_all(&dir);
}
