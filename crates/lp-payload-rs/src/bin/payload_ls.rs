use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;
use lp_payload::{Payload, op_type_name};

#[derive(Parser)]
#[command(
    name = "payload-ls",
    version,
    about = "List / extract Android OTA payload.bin partitions (CrAU + DeltaArchiveManifest)"
)]
struct Args {
    /// Path to payload.bin
    payload: PathBuf,

    /// Extract one partition (raw/bz/xz/zstd/zero/discard ops only)
    #[arg(long, short = 'x')]
    extract: Option<String>,

    /// Extract every partition whose ops are all supported
    #[arg(long, short = 'a')]
    extract_all: Option<PathBuf>,

    /// Output file for --extract (default: <partition>.img in CWD)
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,

    /// Print the manifest partition table as JSON lines
    #[arg(long)]
    json: bool,
}

fn table(payload: &Payload<std::fs::File>, json: bool) {
    let h = payload.header();
    if !json {
        println!(
            "payload v{} | header {} B | manifest {} B | data starts at {} | block_size {}",
            h.major_version,
            h.header_size,
            h.manifest_size,
            payload.data_start(),
            payload.block_size()
        );
        println!(
            "metadata-signature blob: {} B | signatures @manifest-offset {} +{} B",
            h.metadata_signature_size,
            payload.manifest().signatures_offset.unwrap_or(0),
            payload.manifest().signatures_size.unwrap_or(0)
        );
        println!(
            "{:<18} {:>14} {:>6} {:>14}  OP_TYPES",
            "PARTITION", "NEW_SIZE", "OPS", "DATA_BYTES"
        );
    }
    let mut total_ops = 0usize;
    for p in payload.partitions() {
        let new_size = p
            .new_partition_info
            .as_ref()
            .and_then(|i| i.size)
            .unwrap_or(0);
        let data: u64 = p
            .operations
            .iter()
            .map(|o| o.data_length.unwrap_or(0))
            .sum();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for o in &p.operations {
            *counts.entry(op_type_name(o)).or_default() += 1;
        }
        let types = counts
            .iter()
            .map(|(t, c)| format!("{t}x{c}"))
            .collect::<Vec<_>>()
            .join(",");
        total_ops += p.operations.len();
        if json {
            println!(
                "{{\"partition\":\"{}\",\"size\":{},\"ops\":{},\"data\":{},\"types\":\"{}\"}}",
                p.partition_name,
                new_size,
                p.operations.len(),
                data,
                types
            );
        } else {
            println!(
                "{:<18} {:>14} {:>6} {:>14}  {}",
                p.partition_name,
                new_size,
                p.operations.len(),
                data,
                types
            );
        }
    }
    if !json {
        println!(
            "{} partitions, {} operations, {} B total op data",
            payload.partitions().len(),
            total_ops,
            payload.total_data_size()
        );
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut payload = Payload::open(&args.payload)?;

    if args.extract.is_none() && args.extract_all.is_none() {
        table(&payload, args.json);
        return Ok(());
    }

    if let Some(name) = &args.extract {
        let out_path = args
            .out
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{name}.img")));
        let n = payload.extract_partition_to_file(name, &out_path)?;
        println!("extracted {name}: {n} B -> {}", out_path.display());
    }

    if let Some(dir) = &args.extract_all {
        std::fs::create_dir_all(dir)?;
        let names: Vec<String> = payload
            .partitions()
            .iter()
            .filter(|p| {
                p.operations
                    .iter()
                    .all(|o| lp_payload::op_kind(o).is_some())
            })
            .map(|p| p.partition_name.clone())
            .collect();
        let skipped = payload.partitions().len() - names.len();
        for name in &names {
            let t0 = std::time::Instant::now();
            let path = dir.join(format!("{name}.img"));
            let n = payload.extract_partition_to_file(name, &path)?;
            println!("  {name}: {} B in {:.1}s", n, t0.elapsed().as_secs_f64());
        }
        println!(
            "extracted {}/{} partitions to {} ({} skipped: delta ops)",
            names.len(),
            names.len() + skipped,
            dir.display(),
            skipped
        );
    }
    Ok(())
}
