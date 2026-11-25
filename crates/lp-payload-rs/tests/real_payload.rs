//! Real-payload integration tests. Opt-in, needs the 8 GB dump:
//!
//! ```sh
//! LP_RS_PAYLOAD=C:/Downloads/.../payload.bin \
//! LP_RS_IMAGES_DIR=D:/dumps/extracted_partitions \
//! cargo test -p lp-payload-rs --release -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lp_payload::{InstallOperation, OpKind, Payload, op_kind, op_type_name};

fn env_path(key: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("{key} is required")))
}

fn sha256_file(path: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut h = <sha2::Sha256 as sha2::Digest>::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha2::Digest::update(&mut h, &buf[..n]);
    }
    Ok(sha2::Digest::finalize(h).to_vec())
}

#[test]
#[ignore = "needs LP_RS_PAYLOAD + LP_RS_IMAGES_DIR (CPH2487 dump)"]
fn cph2487_manifest_and_op_level_verification() {
    let payload_path = env_path("LP_RS_PAYLOAD");
    let images_dir = env_path("LP_RS_IMAGES_DIR");

    let mut payload = Payload::open(&payload_path).expect("open payload");

    if let Ok(expect) = std::env::var("LP_RS_EXPECT_DATA_START") {
        let expect: u64 = expect.parse().unwrap();
        assert_eq!(
            payload.data_start(),
            expect,
            "data_start vs properties METADATA_SIZE"
        );
    }

    // (1) manifest sizes vs dumped files
    let mut checked = 0usize;
    for p in payload.partitions() {
        let base = p
            .partition_name
            .strip_suffix("_a")
            .unwrap_or(&p.partition_name);
        let img = images_dir.join(format!("{base}.img"));
        if !img.exists() {
            continue;
        }
        let want = p
            .new_partition_info
            .as_ref()
            .and_then(|i| i.size)
            .expect("new_partition_info.size");
        let got = std::fs::metadata(&img).unwrap().len();
        assert_eq!(
            want, got,
            "manifest size vs file size for {}",
            p.partition_name
        );
        checked += 1;
    }
    assert!(
        checked >= 40,
        "expected most partitions comparable, got {checked}"
    );
    println!("size cross-check OK for {checked} partitions");

    // (2) pin data_start: raw SHA256 of sample op blobs (compressed bytes).
    // Clone op samples first to release the manifest borrow.
    let samples: Vec<(String, Vec<InstallOperation>)> = payload
        .partitions()
        .iter()
        .take(30)
        .map(|p| {
            (
                p.partition_name.clone(),
                p.operations.iter().take(2).cloned().collect(),
            )
        })
        .collect();
    let mut sampled = 0usize;
    for (name, ops) in &samples {
        for op in ops {
            if op.data_sha256_hash.is_some() {
                let ok = payload
                    .check_op_data(op)
                    .unwrap_or_else(|e| panic!("check_op_data in {name}: {e}"));
                assert!(
                    ok,
                    "raw blob SHA256 mismatch in {name} -> data_start wrong?"
                );
                sampled += 1;
            }
        }
    }
    assert!(sampled >= 20, "only {sampled} ops hash-checked");
    println!(
        "op-level verification: {sampled} blobs match their SHA256 (data_start = {} pinned)",
        payload.data_start()
    );

    // (3) type census sanity: only supported kinds dominate this payload
    let mut unsupported: Vec<String> = Vec::new();
    for p in payload.partitions() {
        for o in &p.operations {
            if op_kind(o).is_none() {
                let n = op_type_name(o);
                if !unsupported.contains(&n) {
                    unsupported.push(n);
                }
            }
        }
    }
    println!("unsupported op kinds present: {unsupported:?}");
}

#[test]
#[ignore = "needs LP_RS_PAYLOAD + LP_RS_IMAGES_DIR; full 8 GB decode, ~minutes"]
fn cph2487_full_dump_cross_hash() {
    let payload_path = env_path("LP_RS_PAYLOAD");
    let images_dir = env_path("LP_RS_IMAGES_DIR");
    let tmp = env_path("LP_RS_TMP_DIR");
    std::fs::create_dir_all(&tmp).unwrap();

    let mut payload = Payload::open(&payload_path).expect("open payload");
    // Collect (name, extractable, disk-path) up front to end the manifest borrow.
    let jobs: Vec<(String, bool, PathBuf)> = payload
        .partitions()
        .iter()
        .map(|p| {
            let supported = p.operations.iter().all(|o| op_kind(o).is_some());
            let base = p
                .partition_name
                .strip_suffix("_a")
                .unwrap_or(&p.partition_name);
            (
                p.partition_name.clone(),
                supported,
                images_dir.join(format!("{base}.img")),
            )
        })
        .collect();
    let mut ok = 0usize;
    let mut skipped = 0usize;
    for (name, supported, disk) in &jobs {
        if !supported || !disk.exists() {
            skipped += 1;
            println!("skip {name} (supported={supported})");
            continue;
        }
        let t0 = std::time::Instant::now();
        let out = tmp.join(format!("{name}.img"));
        let n = payload
            .extract_partition_to_file(name, &out)
            .unwrap_or_else(|e| panic!("extract {name}: {e}"));
        // extract_partition_to_file already verified op + partition hashes
        // against the MANIFEST; additionally compare against the disk image.
        let a = sha256_file(&out).unwrap();
        let b = sha256_file(disk).unwrap();
        assert_eq!(a, b, "{name}: payload-rebuild != extracted_2026 disk image");
        std::fs::remove_file(&out).unwrap();
        println!(
            "{name}: {n} B decoded, payload == disk (sha256 match) in {:.1}s",
            t0.elapsed().as_secs_f64()
        );
        ok += 1;
    }
    println!("FULL DUMP CROSS-HASH: {ok} partitions identical to disk, {skipped} skipped");
    assert!(
        ok >= 40,
        "expected to verify nearly the whole payload, got {ok}"
    );
    assert_eq!(
        payload
            .partitions()
            .iter()
            .filter(|p| p.operations.iter().all(|o| matches!(
                op_kind(o),
                Some(OpKind::Raw)
                    | Some(OpKind::Bz)
                    | Some(OpKind::Xz)
                    | Some(OpKind::Zstd)
                    | Some(OpKind::Zero)
                    | Some(OpKind::Discard)
            )))
            .count(),
        payload.partitions().len(),
        "this payload should be fully extractable by the supported set"
    );
}
