use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use liblp::{flash_partition_table, read_from_image_file};

#[derive(Parser)]
#[command(
    name = "lpflash",
    version,
    about = "Command-line tool for flashing an empty logical partition table image onto a block device or raw super image"
)]
struct Args {
    /// Target block device or raw super image file
    block_device: PathBuf,

    /// Input minimal/empty super image produced by lpmake (super_empty.img)
    img_file: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let metadata = read_from_image_file(&args.img_file)
        .with_context(|| format!("failed to read image file '{}'", args.img_file.display()))?;

    flash_partition_table(&args.block_device, &metadata).with_context(|| {
        format!(
            "failed to flash partition table to '{}'",
            args.block_device.display()
        )
    })?;

    println!("Successfully flashed partition table.");
    Ok(())
}
