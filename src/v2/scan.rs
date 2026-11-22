//! Memory-mapped inputs cut into newline-aligned chunks of whole data lines
//! (D9, O2, O11).

use std::fs::{self, File};
use std::io;
use std::path::Path;

use memchr::memchr;
use memmap2::{Advice, Mmap};

/// Every non-empty `*.csv` in `dir`, mapped read-only. Zero-length files are
/// skipped (they hold no lines, and mapping them fails).
pub fn map_dir(dir: &Path) -> io::Result<Vec<Mmap>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "csv") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    let mut maps = Vec::with_capacity(paths.len());
    for path in paths {
        let file = File::open(&path)?;
        if file.metadata()?.len() == 0 {
            continue;
        }
        // SAFETY: /data is read-only for the whole run; nothing truncates it.
        let map = unsafe { Mmap::map(&file)? };
        let _ = map.advise(Advice::WillNeed);
        maps.push(map);
    }
    Ok(maps)
}

#[inline]
fn find_newline(buf: &[u8]) -> Option<usize> {
    memchr(b'\n', buf)
}

/// Chunks of about `target` bytes holding only whole data lines: the header is
/// excluded, and each chunk ends just after a `\n` or at EOF.
pub fn chunks<T: AsRef<[u8]>>(files: &[T], target: usize) -> Vec<&[u8]> {
    let target = target.max(1);
    let mut out = Vec::new();
    for file in files {
        let buf = file.as_ref();
        let Some(header_end) = find_newline(buf) else {
            continue;
        };
        let rest = &buf[header_end + 1..];
        let mut start = 0;
        while start < rest.len() {
            let mut end = (start + target).min(rest.len());
            if end < rest.len() {
                end = match find_newline(&rest[end - 1..]) {
                    Some(i) => end + i,
                    None => rest.len(),
                };
            }
            out.push(&rest[start..end]);
            start = end;
        }
    }
    out
}

/// The lines of a chunk: only `\n` ends a line; text after the last `\n` is a
/// line when non-empty.
pub fn lines(chunk: &[u8]) -> Lines<'_> {
    Lines { rest: chunk }
}

pub struct Lines<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];

    #[inline]
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            return None;
        }
        match find_newline(self.rest) {
            Some(i) => {
                let line = &self.rest[..i];
                self.rest = &self.rest[i + 1..];
                Some(line)
            }
            None => {
                let line = self.rest;
                self.rest = &[];
                Some(line)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv;

    fn all_lines(files: &[&[u8]], target: usize) -> Vec<Vec<u8>> {
        chunks(files, target)
            .into_iter()
            .flat_map(|c| lines(c).map(<[u8]>::to_vec).collect::<Vec<_>>())
            .collect()
    }

    fn expected(files: &[&[u8]]) -> Vec<Vec<u8>> {
        files
            .iter()
            .flat_map(|f| csv::data_lines(f).map(<[u8]>::to_vec).collect::<Vec<_>>())
            .collect()
    }

    #[test]
    fn chunks_preserve_lines() {
        let files: Vec<&[u8]> = vec![
            b"h\na,1\nbb,22\n\nccc,333\r\nd",
            b"header only\n",
            b"no newline at all",
            b"h\nx\n",
            b"",
            b"h\n\n\n",
            b"h\nlast,line,without,newline",
        ];
        for target in [1, 2, 3, 7, 64, 4096, 32 << 20] {
            assert_eq!(all_lines(&files, target), expected(&files), "target {target}");
        }
    }

    #[test]
    fn chunks_hold_only_whole_lines() {
        let file: &[u8] = b"h\naaaa\nbbbb\ncccc\n";
        for c in chunks(&[file], 3) {
            assert!(c.ends_with(b"\n"));
        }
    }
}
