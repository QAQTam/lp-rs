use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use liblp::{
    LP_PARTITION_ATTR_NONE, LP_PARTITION_ATTR_READONLY, MetadataBuilder, is_empty_super_image,
    read_from_image_file, read_metadata, update_partition_table, update_sparse_super_image,
    write_empty_image_file, write_partition_to_raw_super,
};
use lp_sparse::is_sparse_image;
use lp_tools::get_logical_image_size;

#[derive(Parser)]
#[command(
    name = "lpadd",
    version,
    about = "Command-line tool for adding or replacing partitions in a super.img or super_empty.img"
)]
struct Args {
    /// Mark the partition as read-only
    #[arg(long = "readonly")]
    readonly: bool,

    /// Replace an existing partition and its contents
    #[arg(long = "replace")]
    replace: bool,

    /// Path to super.img (raw or sparse) or super_empty.img
    super_file: PathBuf,

    /// Name of the partition to add
    part_name: String,

    /// Partition group name
    part_group: String,

    /// Optional partition image file to embed into super.img
    image: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let is_empty = is_empty_super_image(&args.super_file)
        .with_context(|| format!("failed to inspect '{}'", args.super_file.display()))?;

    if is_empty && args.image.is_some() {
        bail!("cannot add a partition image to an empty super file");
    }

    let is_sparse = if is_empty {
        false
    } else {
        let mut r = BufReader::new(File::open(&args.super_file)?);
        is_sparse_image(&mut r)?
    };

    let metadata = if is_empty {
        read_from_image_file(&args.super_file)?
    } else {
        read_metadata(&args.super_file, 0)?
    };

    let mut builder = MetadataBuilder::from_metadata(&metadata)?;

    if args.replace {
        if builder.find_partition(&args.part_name).is_none() {
            bail!("could not find partition to replace: {}", args.part_name);
        }
        builder.remove_partition(&args.part_name);
    }

    let attrs = if args.readonly {
        LP_PARTITION_ATTR_READONLY
    } else {
        LP_PARTITION_ATTR_NONE
    };
    builder.add_partition(&args.part_name, &args.part_group, attrs)?;

    if let Some(img_path) = &args.image {
        let img_size = get_logical_image_size(img_path)?;
        builder.resize_partition(&args.part_name, img_size)?;
    }

    let new_metadata = builder.export()?;

    if is_empty {
        write_empty_image_file(&args.super_file, &new_metadata)?;
    } else if is_sparse {
        println!(
            "Updating sparse super image '{}'...",
            args.super_file.display()
        );
        update_sparse_super_image(
            &args.super_file,
            &new_metadata,
            Some(&args.part_name),
            args.image.as_deref(),
        )?;
    } else {
        let slots = new_metadata.geometry.metadata_slot_count.min(2);
        for slot in 0..slots {
            update_partition_table(&args.super_file, &new_metadata, slot)?;
        }
        if let Some(img_path) = &args.image {
            println!("Writing data for partition '{}'...", args.part_name);
            write_partition_to_raw_super(
                &args.super_file,
                &new_metadata,
                &args.part_name,
                img_path,
            )?;
        }
    }

    println!("Done.");
    Ok(())
}
