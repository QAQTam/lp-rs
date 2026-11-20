use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use liblp::{
    LP_HEADER_FLAG_OVERLAYS_ACTIVE, LP_HEADER_FLAG_VIRTUAL_AB_DEVICE, LP_PARTITION_ATTR_DISABLED,
    LP_PARTITION_ATTR_NONE, LP_PARTITION_ATTR_READONLY, LP_PARTITION_ATTR_SLOT_SUFFIXED,
    LP_PARTITION_ATTR_UPDATED, LP_TARGET_TYPE_LINEAR, compute_partition_size, read_metadata,
};

#[derive(Parser)]
#[command(
    name = "lpdump",
    version,
    about = "Command-line tool for dumping Android Logical Partition metadata"
)]
struct Args {
    /// Slot number (0 = a, 1 = b) or suffix (_a / _b)
    #[arg(short = 's', long = "slot", default_value = "0")]
    slot: String,

    /// Print metadata as JSON
    #[arg(short = 'j', long = "json")]
    json: bool,

    /// Path to super image (raw, sparse, or super_empty.img)
    image: PathBuf,
}

fn parse_slot(s: &str) -> Result<u32> {
    match s {
        "a" | "_a" => Ok(0),
        "b" | "_b" => Ok(1),
        other => other
            .parse()
            .with_context(|| format!("invalid slot '{other}' (expected 0, 1, a, b, _a, or _b)")),
    }
}

fn format_attrs(attrs: u32) -> String {
    if attrs == LP_PARTITION_ATTR_NONE {
        return "none".to_string();
    }
    let mut parts = Vec::new();
    if attrs & LP_PARTITION_ATTR_READONLY != 0 {
        parts.push("readonly");
    }
    if attrs & LP_PARTITION_ATTR_SLOT_SUFFIXED != 0 {
        parts.push("slot_suffixed");
    }
    if attrs & LP_PARTITION_ATTR_UPDATED != 0 {
        parts.push("updated");
    }
    if attrs & LP_PARTITION_ATTR_DISABLED != 0 {
        parts.push("disabled");
    }
    parts.join(",")
}

fn main() -> Result<()> {
    let args = Args::parse();
    let slot = parse_slot(&args.slot)?;

    let meta = read_metadata(&args.image, slot)
        .with_context(|| format!("failed to read metadata from '{}'", args.image.display()))?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&meta)?);
        return Ok(());
    }

    println!(
        "Metadata version: {}.{}",
        meta.header.major_version, meta.header.minor_version
    );
    println!(
        "Metadata size: {} bytes",
        meta.header.header_size + meta.header.tables_size
    );
    println!("Metadata max size: {} bytes", meta.geometry.metadata_max_size);
    println!("Metadata slot count: {}", meta.geometry.metadata_slot_count);
    println!(
        "Logical block size: {} bytes",
        meta.geometry.logical_block_size
    );

    let mut flags = Vec::new();
    if meta.header.flags & LP_HEADER_FLAG_VIRTUAL_AB_DEVICE != 0 {
        flags.push("virtual_ab_device");
    }
    if meta.header.flags & LP_HEADER_FLAG_OVERLAYS_ACTIVE != 0 {
        flags.push("overlays_active");
    }
    println!(
        "Header flags: {}",
        if flags.is_empty() {
            "none".to_string()
        } else {
            flags.join(",")
        }
    );

    println!("------------------------");
    println!("Block device table:");
    for dev in &meta.block_devices {
        println!(
            "  Partition name: {} (flags: 0x{:x})",
            dev.partition_name, dev.flags
        );
        println!(
            "  First sector: {} | Size: {} bytes | Alignment: {} | Alignment offset: {}",
            dev.first_logical_sector, dev.size, dev.alignment, dev.alignment_offset
        );
    }

    println!("------------------------");
    println!("Group table:");
    for grp in &meta.groups {
        println!(
            "  Name: {} | Maximum size: {} bytes | Flags: 0x{:x}",
            grp.name, grp.maximum_size, grp.flags
        );
    }

    println!("------------------------");
    println!("Partition table:");
    for part in &meta.partitions {
        let size = compute_partition_size(&meta, part);
        let group_name = meta
            .groups
            .get(part.group_index as usize)
            .map(|g| g.name.as_str())
            .unwrap_or("?");
        println!(
            "  Name: {} | Group: {} | Size: {} bytes | Attributes: {}",
            part.name,
            group_name,
            size,
            format_attrs(part.attributes)
        );
        let start = part.first_extent_index as usize;
        let end = start + part.num_extents as usize;
        for ext in &meta.extents[start..end] {
            if ext.target_type == LP_TARGET_TYPE_LINEAR {
                let dev_name = meta
                    .block_devices
                    .get(ext.target_source as usize)
                    .map(|d| d.partition_name.as_str())
                    .unwrap_or("?");
                println!(
                    "    Extent: linear {dev_name} {}..{} ({} sectors)",
                    ext.target_data,
                    ext.target_data + ext.num_sectors,
                    ext.num_sectors
                );
            } else {
                println!("    Extent: zero ({} sectors)", ext.num_sectors);
            }
        }
    }

    Ok(())
}
