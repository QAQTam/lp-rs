# lp-rs

[![CI](https://github.com/QAQTam/lp-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/QAQTam/lp-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lp-tools-rs.svg)](https://crates.io/crates/lp-tools-rs)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Pure Rust reimplementation of Android's Logical Partition suite (`liblp`, `libsparse`, and `partition_tools`) with **zero C/C++ build dependencies** — compiles natively on Windows (MSVC), Linux, and macOS.

## Workspace Crates

| Crate | Library / Binaries | Purpose |
| :--- | :--- | :--- |
| [`lp-sparse-rs`](crates/lp-sparse-rs) | `lp_sparse` | Streaming `Read + Seek` reader and file-slice `SparseBuilder` (handles >4 GiB images without loading into RAM or writing temp files). |
| [`liblp-rs`](crates/liblp-rs) | `liblp` | Parser, validator, `MetadataBuilder`, `ImageBuilder`, `flash_partition_table`, `update_partition_table` with rich diagnostic errors. |
| [`lp-tools-rs`](crates/lp-tools-rs) | `lpmake`, `lpunpack`, `lpdump`, `lpadd`, `lpflash` | Standalone KISS CLI tools matching AOSP `partition_tools`. |
| [`lp-payload-rs`](crates/lp-payload-rs) | `lp_payload`, `payload-ls` | OTA `payload.bin` reader: `CrAU` header + prost `DeltaArchiveManifest`, extract with `REPLACE`/`_BZ`/`_XZ`/ZSTD/`ZERO`/`DISCARD` + per-op and per-partition SHA256 (delta ops land here next). |

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

## Device Config JSON (`lpmake --config`)

Repetitive geometry (OPPO/OnePlus `qti_dynamic_partitions_*`, Xiaomi `super` groups, …)
belongs in a per-device template instead of a 40-flag command line:

```bash
lpmake --config devices/cph2487.json --images-dir ./extracted_partitions --sparse -o super.img
```

- The config carries block device, metadata (size / slots / Virtual A/B flag), groups,
  and partitions; `image` entries are filenames resolved against `--images-dir`
  (default: the config's own directory).
- Omit `size` and it is derived from the image file; omit `image` for the zero-size
  `*_b` placeholders that Virtual A/B stock metadata carries.
- Templates live in [`devices/`](devices/). Send a PR with your model's
  `lpdump --json` output (or a super metadata dump from a firmware package) to add one.
- `docs/scripts/payload2super.ps1` runs the full OTA flow: dump payload → drop missing
  `my_*` stock images into the dump dir → assemble → verify every extent size against
  the on-disk images. `docs/scripts/flash_super.ps1` flashes the result: one
  `fastboot flash super` + the static partitions of the dump.

## Documentation

- **[payload.bin → super.img 完整实战（中文）](docs/payload2super.zh-CN.md)** —
  材料清单、几何表采集、模板组装、三重验收、fastbootd 刷机与踩坑实录
  （CPH2487 / ColorOS 16 全程实测数据）。
- [`devices/README.md`](devices/README.md) — device template schema & capture guide.
- [`third_party/aosp/PROVENANCE.md`](third_party/aosp/PROVENANCE.md) — vendored AOSP
  references, codegen recipe, and the empirically verified `CrAU` container layout.
- Tutorial scripts: [`docs/scripts/`](docs/scripts/) (`payload2super.ps1`, `flash_super.ps1`).

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for AOSP attribution.
