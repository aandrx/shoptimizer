//! Single-pass CSV line splitter for V2 (S3-4).
//!
//! Same dialect as [`crate::csv::split`], but it finds the syntax bytes with
//! `memchr2` and validates quotes while walking the line, instead of splitting
//! on commas and then rescanning every field for quotes. No allocation and the
//! returned fields borrow `line`.

use memchr::memchr2;

/// Exactly `N` comma-separated fields, or `None` if malformed (wrong count,
/// empty line, a quote anywhere except wrapping a whole field). A field is
/// `None` for null (`""` or empty).
#[inline]
pub fn split<const N: usize>(line: &[u8]) -> Option<[Option<&[u8]>; N]> {
    // An empty line is malformed, not a one-field line.
    if line.is_empty() {
        return None;
    }
    let mut fields: [Option<&[u8]>; N] = [None; N];
    let mut idx = 0;
    let mut start = 0;
    let mut pos = 0;
    // Per-field quote state, reset at every comma: where the first quote was
    // (if any) and how many there were.
    let mut first_quote = usize::MAX;
    let mut quotes = 0usize;

    loop {
        let Some(off) = memchr2(b',', b'"', &line[pos..]) else {
            // No more syntax: the last field runs to the end of the line.
            if idx + 1 != N {
                return None;
            }
            fields[idx] = field(line, start, line.len(), first_quote, quotes)?;
            return Some(fields);
        };
        let p = pos + off;
        if line[p] == b'"' {
            if quotes == 0 {
                first_quote = p;
            }
            quotes += 1;
            pos = p + 1;
            continue;
        }
        // Comma: close the field.
        if idx == N {
            return None;
        }
        fields[idx] = field(line, start, p, first_quote, quotes)?;
        idx += 1;
        start = p + 1;
        pos = p + 1;
        first_quote = usize::MAX;
        quotes = 0;
    }
}

/// One field from `line[start..end]`. A quote is only valid as the first and
/// last byte of the field, with no other quote inside.
#[inline]
fn field(
    line: &[u8],
    start: usize,
    end: usize,
    first_quote: usize,
    quotes: usize,
) -> Option<Option<&[u8]>> {
    let value = if quotes == 0 {
        &line[start..end]
    } else if quotes == 2 && first_quote == start && end > start + 1 && line[end - 1] == b'"' {
        &line[start + 1..end - 1]
    } else {
        return None;
    };
    Some(if value.is_empty() { None } else { Some(value) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv;

    const EXHAUSTIVE_ALPHABET: &[u8] = b",\"a \r";

    /// `v2::split::split::<N>` must agree with the original `csv::split::<N>`.
    #[track_caller]
    fn check<const N: usize>(line: &[u8]) {
        assert_eq!(
            split::<N>(line),
            csv::split::<N>(line),
            "line {line:?}, N = {N}"
        );
    }

    fn exhaustive<const N: usize>() {
        fn recurse<const N: usize>(buf: &mut Vec<u8>, len: usize) {
            if buf.len() == len {
                check::<N>(buf);
                return;
            }
            for &b in EXHAUSTIVE_ALPHABET {
                buf.push(b);
                recurse::<N>(buf, len);
                buf.pop();
            }
        }
        let mut buf = Vec::with_capacity(8);
        for len in 0..=8 {
            recurse::<N>(&mut buf, len);
        }
    }

    #[test]
    fn matches_csv_split_for_every_short_line() {
        exhaustive::<2>();
        exhaustive::<4>();
        exhaustive::<7>();
    }

    #[test]
    fn matches_csv_split_for_random_lines() {
        // Xorshift64*: deterministic, so a failure is reproducible.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        const ALPHABET: &[u8] = b",\"aA09 \r-_:./";
        let mut line = Vec::with_capacity(120);
        for i in 0..1_000_000 {
            line.clear();
            if i % 2 == 0 {
                // Raw random bytes, every length up to 120.
                let len = (next() % 121) as usize;
                for _ in 0..len {
                    line.push(ALPHABET[(next() % ALPHABET.len() as u64) as usize]);
                }
            } else {
                // Field-structured lines: valid quoted/unquoted/empty fields
                // mixed with malformed quote placements.
                let fields = (next() % 9) as usize;
                for f in 0..fields {
                    if f > 0 {
                        line.push(b',');
                    }
                    let n = (next() % 6) as usize;
                    let kind = next() % 6;
                    let mut text = |line: &mut Vec<u8>| {
                        for _ in 0..n {
                            line.push(ALPHABET[(next() % ALPHABET.len() as u64) as usize]);
                        }
                    };
                    match kind {
                        0 => text(&mut line),
                        1 => {
                            line.push(b'"');
                            text(&mut line);
                            line.push(b'"');
                        }
                        2 => {} // empty field
                        3 => {
                            line.push(b'"');
                            text(&mut line);
                        }
                        4 => {
                            text(&mut line);
                            line.push(b'"');
                        }
                        _ => {
                            line.push(b'"');
                            text(&mut line);
                            line.push(b'"');
                            text(&mut line);
                        }
                    }
                }
            }
            check::<2>(&line);
            check::<4>(&line);
            check::<7>(&line);
        }
    }

    #[test]
    fn fields_borrow_the_input() {
        let line = b"u1,42".to_vec();
        let fields = split::<2>(&line).unwrap();
        let base = line.as_ptr() as usize;
        assert_eq!(fields[0].unwrap().as_ptr() as usize - base, 0);
        assert_eq!(fields[1].unwrap().as_ptr() as usize - base, 3);
    }

    #[test]
    fn dialect_traps() {
        assert_eq!(split::<1>(b""), None);
        assert_eq!(split::<3>(b"a,b"), None);
        assert_eq!(split::<2>(b"a,b,c"), None);
        assert_eq!(split::<2>(b"a,"), Some([Some(&b"a"[..]), None]));
        assert_eq!(split::<1>(b"\"\""), Some([None]));
        assert_eq!(split::<1>(b"\"a\"b"), None);
        assert_eq!(split::<1>(b"a\"b"), None);
        assert_eq!(split::<1>(b"\""), None);
        assert_eq!(split::<1>(b"1000\r"), Some([Some(&b"1000\r"[..])]));
    }
}
