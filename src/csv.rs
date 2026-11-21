//! CSV dialect per `rules.md` "CSV dialect".
//!
//! Implemented by T02.

/// Data lines of one file's bytes: skips the first line (header); only `b'\n'`
/// ends a line; text after the last `\n` is a line if non-empty; a trailing
/// `\r` stays in the line.
pub fn data_lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    let rest = match buf.iter().position(|&b| b == b'\n') {
        Some(i) => &buf[i + 1..],
        // A header without a terminating `\n` is the whole file: no data lines.
        None => &[],
    };
    Lines { rest }
}

/// Iterator over the data lines of a byte slice; `rest` is the unread tail.
struct Lines<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            // Nothing left: text after the final `\n` is not a line when empty.
            return None;
        }
        match self.rest.iter().position(|&b| b == b'\n') {
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

/// Exactly N comma-separated fields, or `None` if malformed (wrong count, empty
/// line, a quote anywhere except wrapping a whole field). Field is `None` for
/// null (`""` or empty).
pub fn split<const N: usize>(line: &[u8]) -> Option<[Option<&[u8]>; N]> {
    // An empty line is malformed, not a one-field line.
    if line.is_empty() {
        return None;
    }
    let mut fields: [Option<&[u8]>; N] = [None; N];
    let mut idx = 0;
    let mut start = 0;
    for i in 0..=line.len() {
        if i == line.len() || line[i] == b',' {
            if idx == N {
                return None;
            }
            fields[idx] = field(&line[start..i])?;
            idx += 1;
            start = i + 1;
        }
    }
    if idx != N {
        return None;
    }
    Some(fields)
}

/// One field: `Some(field)` where the field is `None` for null, or `None` when
/// the field is malformed (a quote not wrapping the whole field).
fn field(value: &[u8]) -> Option<Option<&[u8]>> {
    let value = if value.contains(&b'"') {
        // A quote may only wrap the whole field, with no quote in between.
        if value.len() < 2
            || value[0] != b'"'
            || value[value.len() - 1] != b'"'
            || value[1..value.len() - 1].contains(&b'"')
        {
            return None;
        }
        &value[1..value.len() - 1]
    } else {
        value
    };
    Some(if value.is_empty() { None } else { Some(value) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(buf: &[u8]) -> Vec<&[u8]> {
        data_lines(buf).collect()
    }

    #[test]
    fn header_is_skipped() {
        assert_eq!(lines(b"a,b\n1,2\n"), vec![&b"1,2"[..]]);
        assert_eq!(lines(b"a,b\n1,2"), vec![&b"1,2"[..]]);
    }

    #[test]
    fn final_line_without_newline_is_a_line() {
        assert_eq!(lines(b"h\nx\ny"), vec![&b"x"[..], &b"y"[..]]);
    }

    #[test]
    fn header_only_file_yields_nothing() {
        assert_eq!(lines(b"h\n"), Vec::<&[u8]>::new());
        assert_eq!(lines(b"h"), Vec::<&[u8]>::new());
        assert_eq!(lines(b""), Vec::<&[u8]>::new());
    }

    #[test]
    fn blank_lines_vs_terminal_newline() {
        // The empty text after a single trailing \n is not a line.
        assert_eq!(lines(b"h\na\n"), vec![&b"a"[..]]);
        // A blank line before the terminal newline is a line (and malformed).
        assert_eq!(lines(b"h\na\n\n"), vec![&b"a"[..], &b""[..]]);
        // A blank line right after the header.
        assert_eq!(lines(b"h\n\na"), vec![&b""[..], &b"a"[..]]);
        // A header-only file with a trailing blank line.
        assert_eq!(lines(b"h\n\n"), vec![&b""[..]]);
    }

    #[test]
    fn later_line_equal_to_header_is_data() {
        assert_eq!(lines(b"u1,u2\nu1,u2\n"), vec![&b"u1,u2"[..]]);
    }

    #[test]
    fn trailing_cr_stays_in_last_field() {
        assert_eq!(lines(b"h\n1000\r\n"), vec![&b"1000\r"[..]]);
        assert_eq!(lines(b"h\na,b\r\n").len(), 1);
        assert_eq!(lines(b"h\na,b\r\n")[0], &b"a,b\r"[..]);
    }

    #[test]
    fn quoted_field_is_unwrapped() {
        assert_eq!(split::<1>(b"\"u1\""), Some([Some(&b"u1"[..])]));
    }

    #[test]
    fn quoted_empty_field_is_null() {
        assert_eq!(split::<1>(b"\"\""), Some([None]));
    }

    #[test]
    fn empty_field_is_null() {
        assert_eq!(
            split::<3>(b"a,,c"),
            Some([Some(&b"a"[..]), None, Some(&b"c"[..])])
        );
        assert_eq!(split::<2>(b"a,"), Some([Some(&b"a"[..]), None]));
    }

    #[test]
    fn trailing_empty_field_keeps_field_count() {
        assert_eq!(split::<2>(b"a,"), Some([Some(&b"a"[..]), None]));
        assert_eq!(split::<2>(b","), Some([None, None]));
        assert_eq!(
            split::<3>(b"1,2,"),
            Some([Some(&b"1"[..]), Some(&b"2"[..]), None])
        );
    }

    #[test]
    fn wrong_field_count_is_none() {
        assert_eq!(split::<2>(b"a"), None);
        assert_eq!(split::<2>(b"a,b,c"), None);
        assert_eq!(split::<1>(b"a,b"), None);
        assert_eq!(split::<3>(b"a,b"), None);
        assert_eq!(split::<0>(b"a"), None);
        assert_eq!(split::<0>(b""), None);
    }

    #[test]
    fn empty_line_is_none() {
        assert_eq!(split::<1>(b""), None);
    }

    #[test]
    fn partial_quotes_are_none() {
        assert_eq!(split::<1>(b"u1\""), None);
        assert_eq!(split::<1>(b"\"u1"), None);
        assert_eq!(split::<1>(b"u\"1\""), None);
        assert_eq!(split::<1>(b"\"\"x\"\""), None);
        assert_eq!(split::<2>(b"a,\"b"), None);
        assert_eq!(split::<2>(b"a\"\",b"), None);
    }

    #[test]
    fn spaces_are_part_of_the_value() {
        assert_eq!(split::<1>(b" u1"), Some([Some(&b" u1"[..])]));
        assert_eq!(split::<1>(b"u1 "), Some([Some(&b"u1 "[..])]));
        // A space outside a quote means the quote does not wrap the field.
        assert_eq!(split::<1>(b" \" u1\""), None);
        assert_eq!(split::<1>(b"\" u1\""), Some([Some(&b" u1"[..])]));
        assert_eq!(
            split::<2>(b" a , b "),
            Some([Some(&b" a "[..]), Some(&b" b "[..])])
        );
    }

    #[test]
    fn commas_always_separate_fields() {
        assert_eq!(split::<2>(b"\"a,b\""), None);
        assert_eq!(
            split::<3>(b"\"a\",\"b\",c"),
            Some([Some(&b"a"[..]), Some(&b"b"[..]), Some(&b"c"[..])])
        );
    }

    #[test]
    fn fields_borrow_the_input() {
        let line = b"u1,42".to_vec();
        let fields = split::<2>(&line).unwrap();
        let base = line.as_ptr() as usize;
        assert_eq!(fields[0].unwrap().as_ptr() as usize - base, 0);
        assert_eq!(fields[1].unwrap().as_ptr() as usize - base, 3);
    }
}
