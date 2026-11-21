//! V1 single-threaded pipeline.
//!
//! Implemented by T05.

use std::io;
use std::path::Path;

/// events -> dedup -> validate -> late -> people -> sessions -> credit/assists
/// -> aggregate -> write.
pub fn run(_data: &Path, _out: &Path) -> io::Result<()> {
    todo!("T05: v1::run")
}
