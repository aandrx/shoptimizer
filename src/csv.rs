//! CSV dialect per `rules.md` "CSV dialect".
//!
//! Implemented by T02.

/// Data lines of one file's bytes: skips the first line (header); only `b'\n'`
/// ends a line; text after the last `\n` is a line if non-empty; a trailing
/// `\r` stays in the line.
pub fn data_lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    let _ = buf;
    // Stub until T02: an empty iterator (opaque return types cannot host `todo!()`).
    std::iter::empty()
}

/// Exactly N comma-separated fields, or `None` if malformed (wrong count, empty
/// line, a quote anywhere except wrapping a whole field). Field is `None` for
/// null (`""` or empty).
pub fn split<const N: usize>(line: &[u8]) -> Option<[Option<&[u8]>; N]> {
    let _ = line;
    todo!("T02: csv::split")
}
