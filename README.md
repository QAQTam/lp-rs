# lp-rs

Pure Rust reimplementation of Android's Logical Partition suite (`liblp`, `libsparse`, and `partition_tools`) with **zero C/C++ build dependencies** — compiles natively on Windows (MSVC), Linux, and macOS.

## Workspace Crates

| Crate | Library / Binaries | Purpose |
| :--- | :--- | :--- |
| [`lp-sparse-rs`](crates/lp-sparse-rs) | `lp_sparse` | Streaming `Read + Seek` reader and file-slice `SparseBuilder` (handles >4 GiB images without loading into RAM or writing temp files). |
| [`liblp-rs`](crates/liblp-rs) | `liblp` | Parser, validator, `MetadataBuilder`, `ImageBuilder`, `flash_partition_table`, `update_partition_table` with rich diagnostic errors. |
| [`lp-tools-rs`](crates/lp-tools-rs) | `lpmake`, `lpunpack`, `lpdump`, `lpadd`, `lpflash` | Standalone KISS CLI tools matching AOSP `partition_tools`. |

## Highlights vs. Official AOSP C++ Tools

- **Cross-platform out of the box**: Builds with plain `cargo build --release` on Windows MSVC, Linux, and macOS (no AOSP Soong tree, `libbase`, `libext4_utils`, or `openssl` required).
- **Natively sparse-aware**: `lpdump`, `lpunpack`, and `lpadd` operate directly on Android sparse `super.img` files without forcing you to `simg2img` 10+ GiB images first.
- **Zero huge temp files**: `lpmake` streams raw and sparse input partition images directly via file-offset slicing.
- **Diagnostic errors**: Reports exact byte/hex offsets, expected vs. actual magic/SHA-256 prefixes, and remaining/deficit space in bytes and MiB.
- **Real-device tested**: Verified on a OnePlus Ace 2 (`PHK110`, Virtual A/B, 17 dynamic partitions including >4 GiB `my_stock_a`) flashing `super.img` via `fastboot` and booting cleanly.

## Installation

```bash
cargo install lp-tools-rs
```

Or build from source:

```bash
cargo build --release
```

Binaries will be placed in `target/release/`: `lpmake`, `lpunpack`, `lpdump`, `lpadd`, `lpflash`.

## Quick Examples

```bash
# Inspect a raw, sparse, or super_empty.img (supports --json and --slot)
lpdump super.img
lpdump --json --slot 0 super.img

# Unpack partitions directly from raw or sparse super.img
lpunpack super.img out_dir/
lpunpack -p system_a -p vendor_a super.img out_dir/

# Create a minimal super_empty.img
lpmake --device-size 15032385536 --metadata-size 65536 --metadata-slots 2 --virtual-ab \
  -g qti_dynamic_partitions_a:15028191232 \
  -p system_a:readonly:0:qti_dynamic_partitions_a \
  -o super_empty.img

# Build a full sparse super.img (auto-deduces partition size from image when size=0)
lpmake --device-size 15032385536 --metadata-size 65536 --metadata-slots 2 --virtual-ab --sparse \
  -g qti_dynamic_partitions_a:15028191232 \
  -p system_a:readonly:0:qti_dynamic_partitions_a -i system_a=system.img \
  -p vendor_a:readonly:0:qti_dynamic_partitions_a -i vendor_a=vendor.img \
  -o super.img

# Add or replace a partition inside super.img or super_empty.img
lpadd --readonly --replace super.img vendor_a qti_dynamic_partitions_a vendor.img

# Flash super_empty.img metadata onto a block device or raw super image
lpflash /dev/block/by-name/super super_empty.img
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for AOSP attribution.
