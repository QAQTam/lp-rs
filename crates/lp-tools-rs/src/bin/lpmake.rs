use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use liblp::{
    BlockDeviceInfo, LP_DEFAULT_PARTITION_ALIGNMENT, LP_METADATA_GEOMETRY_SIZE,
    LP_PARTITION_ATTR_NONE, LP_PARTITION_ATTR_READONLY, LP_PARTITION_RESERVED_BYTES,
    MetadataBuilder, write_empty_image_file, write_split_image_files, write_to_image_file,
};
use lp_tools::get_logical_image_size;

#[derive(Parser)]
#[command(
    name = "lpmake",
    version,
    about = "Command-line tool for creating Android Logical Partition (super) images"
)]
struct Args {
    /// Size of the super block device in bytes, or 'auto'
    #[arg(short = 'd', long = "device-size")]
    device_size: Option<String>,

    /// Maximum size reserved for partition metadata
    #[arg(short = 'm', long = "metadata-size")]
    metadata_size: u32,

    /// Number of metadata slots (1 for non-A/B, 2 for A/B)
    #[arg(short = 's', long = "metadata-slots")]
    metadata_slots: u32,

    /// Add a partition: `<name>:<readonly|none>:<size>[:group]`
    #[arg(short = 'p', long = "partition", required = true)]
    partitions: Vec<String>,

    /// Output file path (or directory for multi-device split builds)
    #[arg(short = 'o', long = "output")]
    output: PathBuf,

    /// Physical block size in bytes
    #[arg(short = 'b', long = "block-size", default_value_t = 4096)]
    block_size: u32,

    /// Optimal partition alignment in bytes
    #[arg(short = 'a', long = "alignment")]
    alignment: Option<u32>,

    /// Alignment offset in bytes
    #[arg(short = 'O', long = "alignment-offset")]
    alignment_offset: Option<u32>,

    /// Output an Android sparse image
    #[arg(short = 'S', long = "sparse")]
    sparse: bool,

    /// Associate a partition image: `<partition>=<file>`
    #[arg(short = 'i', long = "image")]
    images: Vec<String>,

    /// Define a partition group: `<group>:<max_size>`
    #[arg(short = 'g', long = "group")]
    groups: Vec<String>,

    /// Add a block device: `<name>:<size>[:alignment[:alignment_offset]]`
    #[arg(short = 'D', long = "device")]
    devices: Vec<String>,

    /// Name of the primary super block device
    #[arg(short = 'n', long = "super-name", default_value = "super")]
    super_name: String,

    /// Mark block device, groups, and partitions as requiring slot suffixes
    #[arg(short = 'x', long = "auto-slot-suffixing")]
    auto_slot_suffixing: bool,

    /// Force writing a full image even if no --image arguments are given
    #[arg(short = 'F', long = "force-full-image")]
    force_full_image: bool,

    /// Set VIRTUAL_AB_DEVICE flag in metadata header (requires v1.2)
    #[arg(long = "virtual-ab")]
    virtual_ab: bool,
}

struct PartitionSpec {
    name: String,
    size: u64,
    attrs: u32,
    group: String,
}

fn parse_partition_spec(s: &str) -> Result<PartitionSpec> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 3 || parts.len() > 4 {
        bail!("invalid --partition format '{s}' (expected <name>:<readonly|none>:<size>[:group])");
    }
    let name = parts[0].to_string();
    if name.is_empty() {
        bail!("partition name cannot be empty");
    }
    let attrs = match parts[1] {
        "none" => LP_PARTITION_ATTR_NONE,
        "readonly" => LP_PARTITION_ATTR_READONLY,
        other => {
            bail!("unrecognized partition attribute '{other}' (expected 'none' or 'readonly')")
        }
    };
    let size: u64 = parts[2]
        .parse()
        .with_context(|| format!("invalid partition size '{}'", parts[2]))?;
    let group = if parts.len() >= 4 {
        parts[3].to_string()
    } else {
        "default".to_string()
    };
    Ok(PartitionSpec {
        name,
        size,
        attrs,
        group,
    })
}

fn calculate_auto_device_size(
    alignment: u32,
    metadata_size: u32,
    metadata_slots: u32,
    partitions: &[PartitionSpec],
) -> Result<u64> {
    let mut ret = LP_PARTITION_RESERVED_BYTES
        + (LP_METADATA_GEOMETRY_SIZE as u64) * 2
        + (metadata_slots as u64) * (metadata_size as u64) * 2;
    if alignment > 0 {
        let align = alignment as u64;
        let rem = ret % align;
        ret = ret
            .checked_add(align - rem)
            .context("overflow aligning metadata size")?;
    }
    ret += (partitions.len() as u64) * (alignment as u64);
    for p in partitions {
        ret += p.size;
    }
    Ok(ret)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut part_specs = Vec::with_capacity(args.partitions.len());
    for p in &args.partitions {
        part_specs.push(parse_partition_spec(p)?);
    }

    let mut images = HashMap::new();
    for spec in &args.images {
        let Some((name, path)) = spec.split_once('=') else {
            bail!("invalid --image '{spec}' (expected PARTITION=FILE)");
        };
        if name.is_empty() || path.is_empty() {
            bail!("invalid --image '{spec}' (expected PARTITION=FILE)");
        }
        images.insert(name.to_string(), PathBuf::from(path));
    }

    for p in &mut part_specs {
        if p.size == 0
            && let Some(img_path) = images.get(&p.name)
        {
            p.size = get_logical_image_size(img_path)?;
        }
    }

    let has_implied_super =
        args.device_size.is_some() || args.alignment.is_some() || args.alignment_offset.is_some();
    let alignment = args.alignment.unwrap_or(LP_DEFAULT_PARTITION_ALIGNMENT);
    let alignment_offset = args.alignment_offset.unwrap_or(0);

    let mut block_devices = Vec::new();
    if has_implied_super {
        if !args.devices.is_empty() {
            bail!("cannot specify both --device-size/--alignment and --device");
        }
        let dev_size_str = args
            .device_size
            .as_deref()
            .context("must specify --device-size or --device")?;
        let dev_size = if dev_size_str == "auto" {
            calculate_auto_device_size(
                alignment,
                args.metadata_size,
                args.metadata_slots,
                &part_specs,
            )?
        } else {
            let v: u64 = dev_size_str
                .parse()
                .context("invalid --device-size value")?;
            if v == 0 {
                bail!("--device-size must be > 0");
            }
            v
        };
        block_devices.push(BlockDeviceInfo::new(
            &args.super_name,
            dev_size,
            alignment,
            alignment_offset,
            args.block_size,
        ));
    } else {
        if args.devices.is_empty() {
            bail!("must specify --device OR --device-size");
        }
        for d in &args.devices {
            let parts: Vec<&str> = d.split(':').collect();
            if parts.len() < 2 || parts.len() > 4 {
                bail!("invalid --device '{d}' (expected <name>:<size>[:alignment[:offset]])");
            }
            let name = parts[0];
            let size: u64 = parts[1].parse().context("invalid block device size")?;
            if size == 0 {
                bail!("block device size must be > 0");
            }
            let align = if parts.len() >= 3 {
                parts[2].parse().context("invalid block device alignment")?
            } else {
                LP_DEFAULT_PARTITION_ALIGNMENT
            };
            let align_off = if parts.len() >= 4 {
                parts[3]
                    .parse()
                    .context("invalid block device alignment offset")?
            } else {
                0
            };
            block_devices.push(BlockDeviceInfo::new(
                name,
                size,
                align,
                align_off,
                args.block_size,
            ));
        }
    }

    let mut builder = MetadataBuilder::new(
        &block_devices,
        &args.super_name,
        args.metadata_size,
        args.metadata_slots,
    )?;

    if args.auto_slot_suffixing {
        builder.set_auto_slot_suffixing(true);
    }
    if args.virtual_ab {
        builder.set_virtual_ab_device_flag(true);
    }

    for g in &args.groups {
        let Some((name, size_str)) = g.split_once(':') else {
            bail!("invalid --group '{g}' (expected <group>:<max_size>)");
        };
        if name.is_empty() {
            bail!("group name cannot be empty");
        }
        let max_size: u64 = size_str.parse().context("invalid group maximum size")?;
        builder.add_group(name, max_size)?;
    }

    for p in &part_specs {
        builder.add_partition(&p.name, &p.group, p.attrs)?;
        builder.resize_partition(&p.name, p.size)?;
    }

    let metadata = builder.export()?;

    if !images.is_empty() || args.force_full_image {
        if block_devices.len() == 1 {
            write_to_image_file(
                &args.output,
                &metadata,
                args.block_size,
                &images,
                args.sparse,
            )?;
        } else {
            write_split_image_files(
                &args.output,
                &metadata,
                args.block_size,
                &images,
                args.sparse,
            )?;
        }
    } else {
        write_empty_image_file(&args.output, &metadata)?;
    }

    println!("Wrote {}", args.output.display());
    Ok(())
}
