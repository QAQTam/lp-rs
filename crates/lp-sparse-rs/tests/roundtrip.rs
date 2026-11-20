use std::io::{Cursor, Read, Seek, SeekFrom};

use lp_sparse::{SparseBuilder, SparseError, SparseReader, encode_raw_to_sparse, is_sparse_image};

#[test]
fn sparse_builder_and_reader_roundtrip_with_crc() {
    let block_size = 4096u32;
    let total_size = 8 * 4096u64;

    let mut builder = SparseBuilder::new(block_size, total_size).unwrap();
    let block1_data = vec![0xABu8; 4096];
    builder.add_data(1, block1_data.clone()).unwrap();
    builder.add_fill(3, 0x1234_5678, 2 * 4096).unwrap();

    let mut sparse_bytes = Vec::new();
    builder.write_sparse(&mut sparse_bytes, true).unwrap();

    let mut cursor = Cursor::new(&sparse_bytes);
    assert!(is_sparse_image(&mut cursor).unwrap());

    let mut reader = SparseReader::open(cursor).unwrap();
    assert_eq!(reader.block_size(), block_size);
    assert_eq!(reader.total_size(), total_size);

    // Block 0: DontCare -> zeroes
    let mut blk0 = [0xFFu8; 4096];
    reader.read_exact(&mut blk0).unwrap();
    assert!(blk0.iter().all(|&b| b == 0));

    // Block 1: Raw 0xAB
    let mut blk1 = [0u8; 4096];
    reader.read_exact(&mut blk1).unwrap();
    assert_eq!(&blk1[..], &block1_data[..]);

    // Seek to block 3 + 2 bytes, check Fill pattern
    reader.seek(SeekFrom::Start(3 * 4096 + 2)).unwrap();
    let mut two_bytes = [0u8; 2];
    reader.read_exact(&mut two_bytes).unwrap();
    assert_eq!(two_bytes, [0x34, 0x12]);

    // Expand back to raw and check full buffer
    let mut expanded = Cursor::new(Vec::new());
    reader.expand_to(&mut expanded).unwrap();
    let raw = expanded.into_inner();
    assert_eq!(raw.len(), total_size as usize);
    assert_eq!(&raw[4096..8192], &block1_data[..]);
    assert_eq!(&raw[3 * 4096..3 * 4096 + 4], &0x1234_5678u32.to_le_bytes());
    assert!(raw[5 * 4096..].iter().all(|&b| b == 0));
}

#[test]
fn raw_to_sparse_to_raw_roundtrip() {
    let mut raw = vec![0u8; 16 * 4096];
    // Put non-uniform pattern in block 2..4
    for (i, b) in raw[2 * 4096..4 * 4096].iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    // Put uniform fill 0xDEAD_BEEF in block 7..10
    raw[7 * 4096..10 * 4096]
        .as_chunks_mut::<4>()
        .0
        .fill(0xDEAD_BEEFu32.to_le_bytes());

    let mut sparse_out = Vec::new();
    encode_raw_to_sparse(&mut Cursor::new(&raw), &mut sparse_out, 4096, true).unwrap();
    assert!(sparse_out.len() < raw.len());

    let mut reader = SparseReader::open(Cursor::new(&sparse_out)).unwrap();
    let mut raw_back = Cursor::new(Vec::new());
    reader.expand_to(&mut raw_back).unwrap();
    assert_eq!(raw_back.into_inner(), raw);
}

#[test]
fn diagnostic_sparse_errors_have_context() {
    let err = SparseReader::open(Cursor::new(vec![0u8; 64])).unwrap_err();
    match err {
        SparseError::InvalidMagic { found, hint, .. } => {
            assert_eq!(found, 0);
            assert!(hint.contains("zero"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let mut b = SparseBuilder::new(4096, 4 * 4096).unwrap();
    b.add_fill(1, 0, 2 * 4096).unwrap();
    let err = b.add_fill(2, 1, 4096).unwrap_err();
    assert!(matches!(
        err,
        SparseError::OverlappingRegion {
            existing_start: 1,
            existing_end: 3,
            ..
        }
    ));
}
