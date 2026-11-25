# AOSP reference material — provenance

Vendored single files from AOSP `platform/system/update_engine` (branch `main`),
fetched as bare blobs via `gitiles ?format=TEXT` (no repository clone).

| File | Purpose | gitiles blob (sha1, verified with `git hash-object`) | Fetched commit |
|---|---|---|---|
| `update_metadata.proto` | Protobuf schema of the OTA payload manifest (`DeltaArchiveManifest`) — drives prost codegen for `lp-payload-rs` | `6d16da40e53720078b2e4f3689dbb296eb325bee` | `dc84c2552b2d4cf00d2a843cb1c091d99d0499f1` |
| `update_engine_README.md` | Container format spec: `CrAU` header layout, blob signature area, manifest sizes, delta vs full semantics (§ Update Payload File Specification) | `fad6eaa4fbcd393d1a7afc4b2583921d55d6ec46` | `dc84c2552b2d4cf00d2a843cb1c091d99d0499f1` |

Retrieve / re-verify any file (e.g. after an AOSP bump):

```sh
curl -s "https://android.googlesource.com/platform/system/update_engine/+/refs/heads/main/<path>?format=TEXT" \
  | tr -d '\r\n' | base64 -d > <path>
git hash-object <path>   # must equal the blob id above
```

License: Apache-2.0 (see headers in each file; repo NOTICE at crate root covers
AOSP attribution). These files are inputs to codegen/reference only — no AOSP
build system involvement.

Do not edit upstream files in place. If we must diverge (e.g. syntax tweaks for
prost), keep the original and add a patched copy alongside it.

## Regenerating the prost bindings

One-shot recipe (no protoc needed; done 2026-11-25):

```sh
# scratch crate with: protox = "0.10", prost-build = "0.14"
# protox::compile(<update_metadata.proto>) -> prost_build::Config::new()
#   .out_dir(crates/lp-payload-rs/src).compile_fds(fds)
# result: crates/lp-payload-rs/src/chromeos_update_engine.rs
```

The generated file is committed; `lp-payload-rs` depends only on `prost` at
runtime, so end users never need protox/protoc.

## Verified payload layout (empirical, CPH2487)

Android `CrAU` v2 header is 24 B with a **u32** metadata-signature size, and the
`Signatures` blob sits BETWEEN the manifest and the data:
`data_start = header + manifest + metadata_signature_size`. The brillo
`payload_inspector.py` u64/manifest-last lore is WRONG for these payloads —
proven by the first op blob starting exactly at `fd 37 7a 58 5a` (XZ magic) and
60 ops passing SHA256 at that base.

