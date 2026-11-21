//! rules.md "Timestamps".

const DAY_MS: i64 = 86_400_000;

/// Parse a short run of ASCII digits (caller guarantees len <= 4).
#[inline]
fn num(b: &[u8]) -> Option<i64> {
    let mut v = 0i64;
    for &c in b {
        let d = c.wrapping_sub(b'0');
        if d > 9 {
            return None;
        }
        v = v * 10 + d as i64;
    }
    Some(v)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS` (19 bytes) -> epoch ms.
fn parse_civil(s: &[u8]) -> Option<i64> {
    if s.len() != 19
        || s[4] != b'-'
        || s[7] != b'-'
        || s[10] != b'T'
        || s[13] != b':'
        || s[16] != b':'
    {
        return None;
    }
    let y = num(&s[0..4])?;
    let mo = num(&s[5..7])?;
    let d = num(&s[8..10])?;
    let h = num(&s[11..13])?;
    let mi = num(&s[14..16])?;
    let se = num(&s[17..19])?;
    if !(1..=12).contains(&mo) || d < 1 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let dim = match mo {
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > dim {
        return None;
    }
    Some(days_from_civil(y, mo, d) * DAY_MS + h * 3_600_000 + mi * 60_000 + se * 1000)
}

/// F1 / F2 / F3 -> epoch ms UTC. None = bad timestamp (never panics, incl. F3 overflow).
pub fn parse_ts(s: &[u8]) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    if s.iter().all(|c| c.is_ascii_digit()) {
        // F3: leading zeros contribute nothing; overflow -> None.
        let mut v: i64 = 0;
        for &c in s {
            v = v.checked_mul(10)?.checked_add((c - b'0') as i64)?;
        }
        return Some(v);
    }
    match s.len() {
        20 if s[19] == b'Z' => parse_civil(&s[..19]),
        25 if (s[19] == b'+' || s[19] == b'-') && s[22] == b':' => {
            let local = parse_civil(&s[..19])?;
            let oh = num(&s[20..22])?;
            let om = num(&s[23..25])?;
            if oh > 23 || om > 59 {
                return None;
            }
            let off = (oh * 3600 + om * 60) * 1000;
            Some(if s[19] == b'+' { local - off } else { local + off })
        }
        _ => None,
    }
}

/// Days since 1970-01-01 for a UTC epoch-ms instant (floor division).
pub fn day_of(ms: i64) -> i32 {
    ms.div_euclid(DAY_MS) as i32
}

/// Observation window, from manifest.json.
pub struct Window {
    pub start: i64,
    pub end: i64,
}

fn string_value<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let pat = format!("\"{}\"", key);
    let pat = pat.as_bytes();
    let mut i = json.windows(pat.len()).position(|w| w == pat)? + pat.len();
    while i < json.len() && json[i].is_ascii_whitespace() {
        i += 1;
    }
    if json.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while i < json.len() && json[i].is_ascii_whitespace() {
        i += 1;
    }
    if json.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let end = i + json[i..].iter().position(|&c| c == b'"')?;
    Some(&json[i..end])
}

/// Extract `window_start` / `window_end` from manifest.json text.
pub fn parse_manifest(json: &[u8]) -> Option<Window> {
    let start = parse_ts(string_value(json, "window_start")?)?;
    let end = parse_ts(string_value(json, "window_end")?)?;
    Some(Window { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<i64> {
        parse_ts(s.as_bytes())
    }

    #[test]
    fn examples_equal() {
        let v = Some(1791071890000);
        assert_eq!(p("2026-10-03T23:58:10Z"), v);
        assert_eq!(p("2026-10-03T19:58:10-04:00"), v);
        assert_eq!(p("1791071890000"), v);
        assert_eq!(p("0001791071890000"), v);
    }

    #[test]
    fn f3() {
        assert_eq!(p("86400000"), Some(86_400_000));
        assert_eq!(p("0"), Some(0));
        assert_eq!(p("9223372036854775807"), Some(i64::MAX));
        assert_eq!(p("9223372036854775808"), None);
        assert_eq!(p("123456789012345678901"), None);
        assert_eq!(p("99999999999999999999"), None);
        assert_eq!(p("000000000000000000000000001"), Some(1));
        for s in ["1791071890000.0", "1.79e12", "-5", ""] {
            assert_eq!(p(s), None, "{s}");
        }
    }

    #[test]
    fn non_ascii_digits() {
        assert_eq!(p("٢"), None);
        assert_eq!(p("２"), None);
        assert_eq!(p("２026-10-03T23:58:10Z"), None);
        assert_eq!(p("٢026-10-03T23:58:10Z"), None);
        assert_eq!(p("2026-10-03T23:58:1٠Z"), None);
    }

    #[test]
    fn bad_shapes() {
        for s in [
            "2026-10-03T23:58:10.123Z",
            "2026-10-03T23:58:10z",
            "2026-10-03t23:58:10Z",
            "2026-10-03 23:58:10Z",
            "2026-10-03T23:58:10",
            "2026-10-03T23:58:10+0200",
            "2026-10-03T23:58:10+24:00",
            "2026-10-03T23:58:10+05:60",
            "2026-10-03T23:58:10 05:00",
        ] {
            assert_eq!(p(s), None, "{s}");
        }
    }

    #[test]
    fn calendar() {
        assert!(p("2028-02-29T00:00:00Z").is_some());
        assert!(p("2000-02-29T00:00:00Z").is_some());
        assert!(p("2027-02-29T00:00:00Z").is_none());
        assert!(p("2100-02-29T00:00:00Z").is_none());
        assert!(p("2026-13-01T00:00:00Z").is_none());
        assert!(p("2026-00-01T00:00:00Z").is_none());
        assert!(p("2026-01-00T00:00:00Z").is_none());
        assert!(p("2026-02-30T00:00:00Z").is_none());
        assert!(p("2026-04-31T00:00:00Z").is_none());
        assert!(p("2026-10-03T24:00:00Z").is_none());
        assert!(p("2026-10-03T23:60:00Z").is_none());
        assert!(p("2026-10-03T23:59:60Z").is_none());
        assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(p("1970-01-02T00:00:00Z"), Some(86_400_000));
    }

    #[test]
    fn offsets() {
        assert!(p("2026-10-03T12:00:00+23:59").is_some());
        assert_eq!(p("2026-10-03T12:00:00-00:00"), p("2026-10-03T12:00:00Z"));
        assert_eq!(p("2026-10-04T01:30:00+02:00"), p("2026-10-03T23:30:00Z"));
        assert_eq!(p("2026-10-03T12:00:00-04:30"), p("2026-10-03T16:30:00Z"));
    }

    #[test]
    fn days() {
        assert_eq!(day_of(-1), -1);
        assert_eq!(day_of(0), 0);
        assert_eq!(day_of(86_399_999), 0);
        assert_eq!(day_of(86_400_000), 1);
        let d = day_of(p("2026-10-03T23:58:10Z").unwrap());
        assert_eq!(d, day_of(p("2026-10-03T00:00:00Z").unwrap()));
        assert_eq!(d as i64, days_from_civil(2026, 10, 3));
    }

    #[test]
    fn manifest() {
        let a = br#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z"}"#;
        let b = b"{\n  \"window_start\": \"2026-10-01T00:00:00Z\",\n  \"window_end\":\"2026-10-15T00:00:00Z\"\n}\n";
        for j in [&a[..], &b[..]] {
            let w = parse_manifest(j).unwrap();
            assert_eq!(w.start, p("2026-10-01T00:00:00Z").unwrap());
            assert_eq!(w.end, p("2026-10-15T00:00:00Z").unwrap());
        }
        assert!(parse_manifest(b"{}").is_none());
    }
}
