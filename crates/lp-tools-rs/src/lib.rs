use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use lp_sparse::{SparseReader, is_sparse_image};

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Opens a raw or Android sparse image as a unified `Read + Seek` stream of the logical image.
pub fn open_super_stream(path: &Path) -> Result<Box<dyn ReadSeek>> {
    let file = File::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
    let mut reader = BufReader::new(file);
    if is_sparse_image(&mut reader)? {
        Ok(Box::new(SparseReader::open(reader)?))
    } else {
        Ok(Box::new(reader))
    }
}

/// Returns the logical (unsparsed) byte length of a raw or Android sparse image.
pub fn get_logical_image_size(path: &Path) -> Result<u64> {
    let file = File::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
    let mut reader = BufReader::new(file);
    if is_sparse_image(&mut reader)? {
        let sparse = SparseReader::open(reader)?;
        Ok(sparse.total_size())
    } else {
        Ok(reader.seek(SeekFrom::End(0))?)
    }
}
