//! V2 parallel pipeline.
//!
//! Implemented by T08-T11. Falls back to [`crate::v1::run`] when D6's packed-ID
//! flag trips.

use std::io;
use std::path::Path;

pub fn run(_data: &Path, _out: &Path) -> io::Result<()> {
    todo!("T08-T11: v2::run")
}
