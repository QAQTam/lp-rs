use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use liblp::{
    BlockDeviceInfo, LP_DEFAULT_PARTITION_ALIGNMENT, LP_METADATA_GEOMETRY_SIZE,
    LP_PARTITION_ATTR_NONE, LP_PARTITION_ATTR_READONLY, LP_PARTITION_RESERVED_BYTES,
    MetadataBuilder, write_empty_image_file, write_split_image_files, write_to_image_file,
};
use lp_tools::get_logical_image_size;
use serde::Deserialize;

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
    metadata_size: Option<u32>,

    /// Number of metadata slots (1 for non-A/B, 2 for A/B, 3 for Virtual A/B)
    #[arg(short = 's', long = "metadata-slots")]
    metadata_slots: Option<u32>,

    /// Add a partition: `<name>:<readonly|none>:<size>[:group]`
    #[arg(short = 'p', long = "partition")]
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

    /// JSON device config (geometry + groups + partitions), replaces the
    /// `-m/-s/-p/-d/-D/-a/-O` command-line geometry arguments
    #[arg(long = "config")]
    config: Option<PathBuf>,

    /// Directory that `image` filenames in the config resolve against
    /// (defaults to the directory containing the config file)
    #[arg(long = "images-dir")]
    images_dir: Option<PathBuf>,
}

struct PartitionSpec {
    name: String,
    size: u64,
    attrs: u32,
    group: String,
}

/// Everything the builder needs, normalized from either the classic CLI flags
/// or a JSON device config.
struct BuildPlan {
    part_specs: Vec<PartitionSpec>,
    images: HashMap<String, PathBuf>,
    groups: Vec<(String, u64)>,
    block_devices: Vec<BlockDeviceInfo>,
    metadata_size: u32,
    metadata_slots: u32,
    super_name: String,
    block_size: u32,
    virtual_ab: bool,
    auto_slot_suffixing: bool,
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
    let attrs = parse_attrs(parts[1]).with_context(|| format!("in --partition '{s}'"))?;
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

fn parse_attrs(s: &str) -> Result<u32> {
    match s {
        "none" => Ok(LP_PARTITION_ATTR_NONE),
        "readonly" => Ok(LP_PARTITION_ATTR_READONLY),
        other => {
            bail!("unrecognized partition attribute '{other}' (expected 'none' or 'readonly')")
        }
    }
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

fn fill_sizes_from_images(
    part_specs: &mut [PartitionSpec],
    images: &HashMap<String, PathBuf>,
) -> Result<()> {
    for p in part_specs {
        if p.size == 0
            && let Some(img_path) = images.get(&p.name)
        {
            p.size = get_logical_image_size(img_path)
                .with_context(|| format!("deriving size for partition '{}'", p.name))?;
        }
    }
    Ok(())
}

fn plan_from_cli(args: &Args) -> Result<BuildPlan> {
    if args.metadata_size.is_none() || args.metadata_slots.is_none() {
        bail!("--metadata-size and --metadata-slots are required (or use --config)");
    }
    let metadata_size = args.metadata_size.unwrap();
    let metadata_slots = args.metadata_slots.unwrap();

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

    fill_sizes_from_images(&mut part_specs, &images)?;

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
            calculate_auto_device_size(alignment, metadata_size, metadata_slots, &part_specs)?
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
            bail!("must specify --device OR --device-size (or use --config)");
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

    let mut groups = Vec::with_capacity(args.groups.len());
    for g in &args.groups {
        let Some((name, size_str)) = g.split_once(':') else {
            bail!("invalid --group '{g}' (expected <group>:<max_size>)");
        };
        if name.is_empty() {
            bail!("group name cannot be empty");
        }
        let max_size: u64 = size_str.parse().context("invalid group maximum size")?;
        groups.push((name.to_string(), max_size));
    }

    Ok(BuildPlan {
        part_specs,
        images,
        groups,
        block_devices,
        metadata_size,
        metadata_slots,
        super_name: args.super_name.clone(),
        block_size: args.block_size,
        virtual_ab: args.virtual_ab,
        auto_slot_suffixing: args.auto_slot_suffixing,
    })
}

// ---------------------------------------------------------------------------
// JSON device config
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LpConfig {
    /// Free-form note carried alongside the device template (ignored).
    #[serde(default)]
    _comment: Option<String>,
    device: DeviceCfg,
    metadata: MetadataCfg,
    #[serde(default)]
    groups: Vec<GroupCfg>,
    partitions: Vec<PartCfg>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceCfg {
    #[serde(default = "default_super_name")]
    name: String,
    size: u64,
    alignment: Option<u32>,
    alignment_offset: Option<u32>,
    block_size: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataCfg {
    size: u32,
    slots: u32,
    #[serde(default)]
    virtual_ab: bool,
    #[serde(default)]
    auto_slot_suffixing: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupCfg {
    name: String,
    maximum_size: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartCfg {
    name: String,
    group: Option<String>,
    attrs: Option<String>,
    size: Option<u64>,
    /// Filename resolved against --images-dir (or the config's own directory).
    /// Omit for zero-size placeholder partitions (e.g. `*_b` on Virtual A/B).
    image: Option<String>,
}

fn default_super_name() -> String {
    "super".to_string()
}

fn plan_from_config(args: &Args) -> Result<BuildPlan> {
    let cfg_path = args.config.as_ref().unwrap();

    // Geometry comes from the config; refuse silent conflicts with legacy flags.
    let mut conflicts: Vec<&str> = Vec::new();
    if !args.partitions.is_empty() {
        conflicts.push("--partition");
    }
    if args.metadata_size.is_some() {
        conflicts.push("--metadata-size");
    }
    if args.metadata_slots.is_some() {
        conflicts.push("--metadata-slots");
    }
    if args.device_size.is_some() {
        conflicts.push("--device-size");
    }
    if !args.devices.is_empty() {
        conflicts.push("--device");
    }
    if args.alignment.is_some() {
        conflicts.push("--alignment");
    }
    if args.alignment_offset.is_some() {
        conflicts.push("--alignment-offset");
    }
    if !conflicts.is_empty() {
        bail!(
            "--config carries its own geometry; remove: {}",
            conflicts.join(", ")
        );
    }

    let text = std::fs::read_to_string(cfg_path)
        .with_context(|| format!("reading config '{}'", cfg_path.display()))?;
    let cfg: LpConfig =
        serde_json::from_str(&text).with_context(|| format!("parsing '{}'", cfg_path.display()))?;

    if cfg.partitions.is_empty() {
        bail!("config has no partitions");
    }
    if cfg.device.size == 0 {
        bail!("device.size must be > 0");
    }

    let block_size = cfg.device.block_size.unwrap_or(args.block_size);
    let alignment = cfg
        .device
        .alignment
        .unwrap_or(LP_DEFAULT_PARTITION_ALIGNMENT);
    let alignment_offset = cfg.device.alignment_offset.unwrap_or(0);

    let images_base = args
        .images_dir
        .clone()
        .or_else(|| cfg_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    let mut part_specs = Vec::with_capacity(cfg.partitions.len());
    let mut images: HashMap<String, PathBuf> = HashMap::new();

    for p in &cfg.partitions {
        if p.name.is_empty() {
            bail!("config: partition name cannot be empty");
        }
        if images.contains_key(&p.name) {
            bail!("config: duplicate partition '{}'", p.name);
        }
        let attrs = match p.attrs.as_deref() {
            Some(a) => parse_attrs(a).with_context(|| format!("config: partition '{}'", p.name))?,
            None => LP_PARTITION_ATTR_NONE,
        };
        let size = p.size.unwrap_or(0);
        let group = p.group.clone().unwrap_or_else(|| "default".to_string());
        part_specs.push(PartitionSpec {
            name: p.name.clone(),
            size,
            attrs,
            group,
        });
        if let Some(img) = &p.image {
            let path = if Path::new(img).is_absolute() {
                PathBuf::from(img)
            } else {
                images_base.join(img)
            };
            images.insert(p.name.clone(), path);
        }
    }

    // `-i` on the command line still works as an override/associate layer.
    for spec in &args.images {
        let Some((name, path)) = spec.split_once('=') else {
            bail!("invalid --image '{spec}' (expected PARTITION=FILE)");
        };
        if name.is_empty() || path.is_empty() {
            bail!("invalid --image '{spec}' (expected PARTITION=FILE)");
        }
        images.insert(name.to_string(), PathBuf::from(path));
    }

    fill_sizes_from_images(&mut part_specs, &images)?;

    let mut groups = Vec::with_capacity(cfg.groups.len());
    for g in &cfg.groups {
        if g.name.is_empty() {
            bail!("config: group name cannot be empty");
        }
        groups.push((g.name.clone(), g.maximum_size));
    }

    let block_devices = vec![BlockDeviceInfo::new(
        &cfg.device.name,
        cfg.device.size,
        alignment,
        alignment_offset,
        block_size,
    )];

    Ok(BuildPlan {
        part_specs,
        images,
        groups,
        block_devices,
        metadata_size: cfg.metadata.size,
        metadata_slots: cfg.metadata.slots,
        super_name: cfg.device.name.clone(),
        block_size,
        virtual_ab: cfg.metadata.virtual_ab,
        auto_slot_suffixing: cfg.metadata.auto_slot_suffixing,
    })
}

fn main() -> Result<()> {
    let args = Args::parse();

    let plan = if args.config.is_some() {
        plan_from_config(&args)?
    } else {
        plan_from_cli(&args)?
    };

    let mut builder = MetadataBuilder::new(
        &plan.block_devices,
        &plan.super_name,
        plan.metadata_size,
        plan.metadata_slots,
    )?;

    if plan.auto_slot_suffixing {
        builder.set_auto_slot_suffixing(true);
    }
    if plan.virtual_ab {
        builder.set_virtual_ab_device_flag(true);
    }

    for (name, max_size) in &plan.groups {
        builder.add_group(name, *max_size)?;
    }

    for p in &plan.part_specs {
        builder.add_partition(&p.name, &p.group, p.attrs)?;
        builder.resize_partition(&p.name, p.size)?;
    }

    let metadata = builder.export()?;

    if !plan.images.is_empty() || args.force_full_image {
        if plan.block_devices.len() == 1 {
            write_to_image_file(
                &args.output,
                &metadata,
                plan.block_size,
                &plan.images,
                args.sparse,
            )?;
        } else {
            write_split_image_files(
                &args.output,
                &metadata,
                plan.block_size,
                &plan.images,
                args.sparse,
            )?;
        }
    } else {
        write_empty_image_file(&args.output, &metadata)?;
    }

    println!("Wrote {}", args.output.display());
    Ok(())
}
