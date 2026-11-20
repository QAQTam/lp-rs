use std::env;
use std::fs::File;
use std::io::{BufReader, BufWriter};

use lp_sparse::encode_raw_to_sparse;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: img2simg <raw_input> <sparse_output>");
        std::process::exit(1);
    }
    let mut input = BufReader::new(File::open(&args[1])?);
    let mut output = BufWriter::new(File::create(&args[2])?);
    encode_raw_to_sparse(&mut input, &mut output, 4096, false)?;
    println!("Converted {} -> {}", args[1], args[2]);
    Ok(())
}
