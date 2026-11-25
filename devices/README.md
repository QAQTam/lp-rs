# devices/ — per-model super geometry templates

`lpmake --config` inputs. One JSON per phone model, capturing the LP
metadata **as shipped on the device** — partition names, groups, limits,
alignment, Virtual A/B flags. Payloads provide the images; this directory
provides the table.

## Schema

```jsonc
{
  "device": {
    "name": "super",            // block device partition name (usually "super")
    "size": 15032385536,        // bytes — MUST equal the device's super partition capacity
    "alignment": 1048576,       // LP extent alignment (1 MiB on modern Qualcomm)
    "alignment_offset": 0,
    "block_size": 4096
  },
  "metadata": {
    "size": 65536,              // metadata_max_size
    "slots": 3,                 // 3 for Virtual A/B, 2 for classic A/B, 1 for legacy
    "virtual_ab": true,         // sets VIRTUAL_AB_DEVICE header flag (needs v1.2)
    "auto_slot_suffixing": false // prefer explicit _a/_b names copied from stock
  },
  "groups": [                   // "default" is built-in; list the device groups only
    { "name": "qti_dynamic_partitions_a", "maximum_size": 15028191232 },
    { "name": "qti_dynamic_partitions_b", "maximum_size": 15028191232 }
  ],
  "partitions": [
    // attrs: "readonly" | "none";  size omitted = derived from the image file
    // image: filename resolved against --images-dir (payload dump base name,
    //        WITHOUT slot suffix — the table name carries it)
    // *_b entries: omit image (zero-size placeholders, exactly like stock)
    { "name": "system_a", "attrs": "readonly", "group": "qti_dynamic_partitions_a", "image": "system.img" },
    { "name": "system_b", "attrs": "readonly", "group": "qti_dynamic_partitions_b" }
  ]
}
```

Unknown fields are rejected (`deny_unknown_fields`), except a free-form
`_comment` string.

## How to capture the table (pick one)

1. **From a firmware/9008 package**: some packages ship a bare super metadata
   block (a few KB, not a full super image) — that IS the table:
   `lpdump --json super_meta.raw`.
2. **From a rooted device** (no space needed for the full super):
   ```sh
   adb shell su -c "dd if=/dev/block/by-name/super of=/sdcard/super_head.bin bs=1M count=16"
   # then, on PC:
   lpdump --json super_head.bin
   ```
3. **From any super.img you already have** (raw or Android sparse):
   `lpdump --json super.img`.

## Turning a dump into a template

```bash
lpdump --json super_head.bin > meta.json
```

Then transcribe the three tables into the schema above, keeping **entry
order, names, groups, attrs identical to the dump** — that is the whole
point of the file. Partition `image` entries follow the dump dir base names.
See `cph2487.json` for a complete worked example (OnePlus, ColorOS 16,
17 dynamic partitions incl. my_*).

## Contributing a model

Open a PR with:

- `devices/<model>.json` (set `_comment` with device name, source of the
  dump, Android/ColorOS version it was captured from)
- which OTAs you have assembled with it, and — if you can — the `lpdump`
  output of the assembled image

Geometry from a *different storage SKU* of the same model may differ (super
size often depends on the flash capacity variant) — note the SKU in
`_comment` when known.
