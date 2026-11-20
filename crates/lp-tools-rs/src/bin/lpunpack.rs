use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use liblp::{LP_SECTOR_SIZE, LP_TARGET_TYPE_LINEAR, compute_partition_size, read_metadata};
use lp_tools::{ReadSeek, open_super_stream};

#[derive(Parser)]
#[command(
    name = "lpunpack",
    version,
    about = "Command-line tool for extracting partition images from super (supports raw and sparse)"
)]
struct Args {
    /// Additional super image for split multi-device builds (repeatable)
    #[arg(short = 'i', long = "image")]
    images: Vec<PathBuf>,

    /// Extract only the named partition (repeatable; default extracts all)
    #[arg(short = 'p', long = "partition")]
    partitions: Vec<String>,

    /// Slot number (default 0)
    #[arg(short = 'S', long = "slot", default_value_t = 0)]
    slot: u32,

    /// Path to primary super image (raw or sparse)
    super_image: PathBuf,

    /// Output directory (default '.')
    #[arg(default_value = ".")]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut all_paths = Vec::with_capacity(1 + args.images.len());
    all_paths.push(args.super_image.clone());
    all_paths.extend(args.images);

    let meta = read_metadata(&all_paths[0], args.slot)
        .with_context(|| format!("failed to read metadata from '{}'", all_paths[0].display()))?;

    let mut streams: Vec<Box<dyn ReadSeek>> = Vec::with_capacity(all_paths.len());
    for p in &all_paths {
        streams.push(open_super_stream(p)?);
    }

    fs::create_dir_all(&args.output_dir)?;

    let mut wanted: HashSet<String> = args.partitions.into_iter().collect();
    let extract_all = wanted.is_empty();

    let mut targets = Vec::new();
    for part in &meta.partitions {
        if extract_all || wanted.remove(&part.name) {
            targets.push(part);
        }
    }
    if !extract_all && !wanted.is_empty() {
        let missing = wanted.into_iter().next().unwrap();
        bail!("partition not found in super image: {missing}");
    }

    let block_size = meta.geometry.logical_block_size as usize;
    let mut buf = vec![0u8; block_size];

    for part in targets {
        let total_bytes = compute_partition_size(&meta, part);
        let out_path = args.output_dir.join(format!("{}.img", part.name));
        println!(
            "Extracting '{}' ({total_bytes} bytes) -> {}",
            part.name,
            out_path.display()
        );

        let pb = ProgressBar::new(total_bytes);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})",
            )?
            .progress_chars("#>-"),
        );

        let mut out_file = BufWriter::new(File::create(&out_path)?);
        let mut hole_bytes: u64 = 0;

        let ext_start = part.first_extent_index as usize;
        let ext_end = ext_start + part.num_extents as usize;
        for ext in &meta.extents[ext_start..ext_end] {
            if ext.target_type != LP_TARGET_TYPE_LINEAR {
                bail!("unsupported non-linear extent in partition '{}'", part.name);
            }
            let stream_count = streams.len();
            let stream = streams.get_mut(ext.target_source as usize).with_context(|| {
                format!(
                    "extent references device index {} but only {stream_count} image(s) provided",
                    ext.target_source
                )
            })?;
            stream.seek(SeekFrom::Start(ext.target_data * LP_SECTOR_SIZE))?;

            let mut rem = ext.num_sectors * LP_SECTOR_SIZE;
            while rem > 0 {
                if rem < block_size as u64 {
                    bail!("extent size is not a multiple of block_size ({block_size})");
                }
                stream.read_exact(&mut buf)?;
                if buf.iter().all(|&b| b == 0) {
                    hole_bytes += block_size as u64;
                } else {
                    if hole_bytes > 0 {
                        out_file.seek(SeekFrom::Current(hole_bytes as i64))?;
                        hole_bytes = 0;
                    }
                    out_file.write_all(&buf)?;
                }
                pb.inc(block_size as u64);
                rem -= block_size as u64;
            }
        }

        if hole_bytes > 0 {
            out_file.flush()?;
            let mut inner = out_file.into_inner()?;
            let cur = inner.stream_position()?;
            inner.set_len(cur + hole_bytes)?;
        } else {
            out_file.flush()?;
        }

        pb.finish_and_clear();
    }

    Ok(())
}
