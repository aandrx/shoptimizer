//! Single-threaded reference-quality pipeline:
//! events -> dedup -> validate -> late -> people -> sessions -> credit/assists -> aggregate.

use std::io;
use std::path::Path;

pub fn run(data: &Path, out: &Path) -> io::Result<()> {
    let _ = (data, out);
    todo!()
}
