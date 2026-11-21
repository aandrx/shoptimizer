//! rules.md "CSV dialect".

/// Data lines of one file's bytes: skips the first line (header); only b'\n' ends a line;
/// text after the last '\n' is a line if non-empty; a trailing '\r' stays in the line.
pub fn data_lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    // Dropping one final '\n' makes "text after the last \n" a line only when non-empty.
    let body = buf.strip_suffix(b"\n").unwrap_or(buf);
    body.split(|&b| b == b'\n').skip(1)
}

/// Exactly N comma-separated fields, or None if malformed (wrong count, empty line,
/// a quote anywhere except wrapping a whole field). Field is None for null ("" or empty).
pub fn split<const N: usize>(line: &[u8]) -> Option<[Option<&[u8]>; N]> {
    if line.is_empty() {
        return None;
    }
    let mut out: [Option<&[u8]>; N] = [None; N];
    let mut parts = line.split(|&b| b == b',');
    for slot in out.iter_mut() {
        let mut v = parts.next()?;
        if v.contains(&b'"') {
            if v.len() < 2 || v[0] != b'"' || v[v.len() - 1] != b'"' {
                return None;
            }
            v = &v[1..v.len() - 1];
            if v.contains(&b'"') {
                return None;
            }
        }
        *slot = if v.is_empty() { None } else { Some(v) };
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s3(l: &[u8]) -> Option<[Option<&[u8]>; 3]> {
        split::<3>(l)
    }
    fn s1(l: &[u8]) -> Option<[Option<&[u8]>; 1]> {
        split::<1>(l)
    }
    fn lines(b: &[u8]) -> Vec<&[u8]> {
        data_lines(b).collect()
    }

    #[test]
    fn quoted_and_null() {
        assert_eq!(s1(b"\"u1\""), Some([Some(&b"u1"[..])]));
        assert_eq!(s1(b"\"\""), Some([None]));
        assert_eq!(s3(b"a,,b"), Some([Some(&b"a"[..]), None, Some(&b"b"[..])]));
        assert_eq!(s3(b"\"a\",b,\"\""), Some([Some(&b"a"[..]), Some(&b"b"[..]), None]));
        assert_eq!(s3(b",,"), Some([None, None, None]));
    }

    #[test]
    fn malformed() {
        assert_eq!(s3(b"a,b"), None);
        assert_eq!(s3(b"a,b,c,d"), None);
        assert_eq!(s1(b""), None);
        for l in [&b"u1\""[..], b"\"u1", b"u\"1\"", b"\"\"x\"\"", b"\""] {
            assert_eq!(s1(l), None, "{:?}", l);
        }
    }

    #[test]
    fn no_trim() {
        assert_eq!(s1(b" u1"), Some([Some(&b" u1"[..])]));
        assert_eq!(s1(b"\" u1\""), Some([Some(&b" u1"[..])]));
        assert_eq!(s3(b"a,b,c\r"), Some([Some(&b"a"[..]), Some(&b"b"[..]), Some(&b"c\r"[..])]));
    }

    #[test]
    fn lines_basic() {
        assert_eq!(lines(b"h\na\nb\n"), vec![&b"a"[..], b"b"]);
        assert_eq!(lines(b"h\na\nb"), vec![&b"a"[..], b"b"]);
        assert_eq!(lines(b"h\r\na\r\n"), vec![&b"a\r"[..]]);
        assert!(lines(b"h").is_empty());
        assert!(lines(b"h\n").is_empty());
        assert!(lines(b"").is_empty());
        assert!(lines(b"\n").is_empty());
        assert_eq!(lines(b"h\n\nx\n"), vec![&b""[..], b"x"]);
        assert_eq!(lines(b"h\n\n"), vec![&b""[..]]);
        assert_eq!(lines(b"h\nh\n"), vec![&b"h"[..]]);
    }
}
