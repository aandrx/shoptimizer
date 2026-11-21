//! Timestamps per `rules.md` "Timestamps".
//!
//! Implemented by T03.

const SECOND_MS: i64 = 1_000;
const MINUTE_MS: i64 = 60 * SECOND_MS;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// F1 / F2 / F3 -> epoch ms UTC. `None` = bad timestamp (never panics, including
/// F3 overflow).
///
/// F3 accepts any number of leading zeros; a value that does not fit in `i64`
/// is reported as `None` (checked, never wrapped).
pub fn parse_ts(s: &[u8]) -> Option<i64> {
    // F3: ASCII digits only, any length, leading zeros allowed.
    if !s.is_empty() && s.iter().all(u8::is_ascii_digit) {
        return parse_digits(s);
    }
    // F1: `YYYY-MM-DDTHH:MM:SSZ`.
    if s.len() == 20 && s[19] == b'Z' {
        return parse_civil(&s[..19]);
    }
    // F2: `YYYY-MM-DDTHH:MM:SS+HH:MM` / `-HH:MM`.
    if s.len() == 25 && (s[19] == b'+' || s[19] == b'-') && s[22] == b':' {
        let local = parse_civil(&s[..19])?;
        let hours = parse_two_digits(&s[20..22])?;
        let minutes = parse_two_digits(&s[23..25])?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let offset = hours as i64 * HOUR_MS + minutes as i64 * MINUTE_MS;
        // Subtract the offset to get UTC.
        return if s[19] == b'+' {
            local.checked_sub(offset)
        } else {
            local.checked_add(offset)
        };
    }
    None
}

/// Days since 1970-01-01 for a UTC epoch-ms instant (floor division).
///
/// The result is exact for every `ms` whose UTC day is representable as `i32`:
/// `i32::MIN as i64 * DAY_MS <= ms <= i32::MAX as i64 * DAY_MS + (DAY_MS - 1)`.
/// Outside that domain the result saturates at `i32::MIN` / `i32::MAX` rather
/// than wrapping. Pipeline timestamps inside a manifest window are far inside
/// the valid domain.
pub fn day_of(ms: i64) -> i32 {
    let days = ms.div_euclid(DAY_MS);
    if days < i32::MIN as i64 {
        i32::MIN
    } else if days > i32::MAX as i64 {
        i32::MAX
    } else {
        days as i32
    }
}

/// Event/click window from `manifest.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: i64,
    pub end: i64,
}

impl Window {
    /// Parses the documented `manifest.json` object:
    /// `{"window_start": "...", "window_end": "..."}`.
    ///
    /// Whitespace (including newlines) and key order are free. Both values must
    /// be JSON strings that `parse_ts` accepts. Unknown string-valued keys are
    /// ignored; duplicate or missing bounds, escapes, non-string values and any
    /// trailing content are rejected. This is deliberately not a general JSON
    /// implementation.
    pub fn parse(manifest: &[u8]) -> Option<Self> {
        let mut json = Json {
            bytes: manifest,
            pos: 0,
        };
        json.skip_ws();
        json.expect(b'{')?;
        json.skip_ws();

        let mut start = None;
        let mut end = None;
        if json.peek() == Some(b'}') {
            json.pos += 1;
        } else {
            loop {
                json.skip_ws();
                let key = json.string()?;
                json.skip_ws();
                json.expect(b':')?;
                json.skip_ws();
                let value = json.string()?;
                match key {
                    b"window_start" if start.replace(value).is_some() => return None,
                    b"window_end" if end.replace(value).is_some() => return None,
                    _ => {}
                }
                json.skip_ws();
                match json.next()? {
                    b',' => {}
                    b'}' => break,
                    _ => return None,
                }
            }
        }

        json.skip_ws();
        if json.pos != json.bytes.len() {
            return None;
        }
        Some(Self {
            start: parse_ts(start?)?,
            end: parse_ts(end?)?,
        })
    }
}

/// Minimal scanner for the manifest object; values are strings only.
struct Json<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Json<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn expect(&mut self, expected: u8) -> Option<()> {
        if self.next()? == expected {
            Some(())
        } else {
            None
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        if self.next()? != b'"' {
            return None;
        }
        let start = self.pos;
        loop {
            match self.next()? {
                b'"' => return Some(&self.bytes[start..self.pos - 1]),
                // The documented manifest has no escapes, so none are supported.
                b'\\' => return None,
                0x00..=0x1f => return None,
                _ => {}
            }
        }
    }
}

/// Parses `YYYY-MM-DDTHH:MM:SS` (19 bytes) as a UTC civil time.
fn parse_civil(s: &[u8]) -> Option<i64> {
    if s.len() != 19 {
        return None;
    }
    if s[4] != b'-' || s[7] != b'-' || s[10] != b'T' || s[13] != b':' || s[16] != b':' {
        return None;
    }
    let year = parse_fixed_digits(&s[0..4])? as i64;
    let month = parse_fixed_digits(&s[5..7])? as i64;
    let day = parse_fixed_digits(&s[8..10])? as i64;
    let hour = parse_fixed_digits(&s[11..13])? as i64;
    let minute = parse_fixed_digits(&s[14..16])? as i64;
    let second = parse_fixed_digits(&s[17..19])? as i64;

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    Some(
        days_from_civil(year, month, day) * DAY_MS
            + hour * HOUR_MS
            + minute * MINUTE_MS
            + second * SECOND_MS,
    )
}

/// Parses a non-empty ASCII digit string as `i64`, rejecting overflow.
fn parse_digits(s: &[u8]) -> Option<i64> {
    let mut value: i64 = 0;
    for &byte in s {
        value = value.checked_mul(10)?.checked_add((byte - b'0') as i64)?;
    }
    Some(value)
}

/// Parses exactly two ASCII digits (0..=99).
fn parse_two_digits(s: &[u8]) -> Option<u32> {
    if s.len() != 2 {
        return None;
    }
    parse_fixed_digits(s)
}

/// Parses a short run of ASCII digits (at most 9 bytes, so it cannot overflow).
fn parse_fixed_digits(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 9 {
        return None;
    }
    let mut value: u32 = 0;
    for &byte in s {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (byte - b'0') as u32;
    }
    Some(value)
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Civil date -> days since 1970-01-01 (Howard Hinnant's algorithm).
///
/// Uses floor division so year 0000 (and its Jan/Feb) matches the Python oracle.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f1_f2_f3_examples_are_the_same_instant() {
        let expected = Some(1_791_071_890_000);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10Z"), expected);
        assert_eq!(parse_ts(b"2026-10-03T19:58:10-04:00"), expected);
        assert_eq!(parse_ts(b"1791071890000"), expected);
    }

    #[test]
    fn f3_leading_zeros_and_short_values() {
        assert_eq!(parse_ts(b"0001791071890000"), Some(1_791_071_890_000));
        assert_eq!(
            parse_ts(b"0000000000000000000000000000001791071890000"),
            Some(1_791_071_890_000)
        );
        // Out of window is decided elsewhere; this is a valid F3 number.
        assert_eq!(parse_ts(b"86400000"), Some(86_400_000));
        assert_eq!(parse_ts(b"0"), Some(0));
    }

    #[test]
    fn f3_overflow_is_none_and_never_panics() {
        assert_eq!(parse_ts(b"999999999999999999999"), None); // 21 digits
        assert_eq!(parse_ts(b"9223372036854775808"), None); // i64::MAX + 1
        assert_eq!(
            parse_ts(b"9223372036854775807"),
            Some(i64::MAX),
            "exactly i64::MAX is still representable"
        );
    }

    #[test]
    fn f3_rejects_every_non_digit() {
        assert_eq!(parse_ts(b"1791071890000.0"), None);
        assert_eq!(parse_ts(b"1.79e12"), None);
        assert_eq!(parse_ts(b"-5"), None);
        assert_eq!(parse_ts(b"+5"), None);
        assert_eq!(parse_ts(b"1791071890000 "), None);
        assert_eq!(parse_ts(b"1791071890000\r"), None);
    }

    #[test]
    fn non_ascii_digits_are_rejected() {
        assert_eq!(parse_ts("٢٥٠".as_bytes()), None);
        assert_eq!(parse_ts("２５０".as_bytes()), None);
        assert_eq!(parse_ts("2026-10-03T23:58:10Z\u{0662}".as_bytes()), None);
    }

    #[test]
    fn malformed_civil_forms_are_rejected() {
        assert_eq!(parse_ts(b""), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10.500Z"), None); // fractional seconds
        assert_eq!(parse_ts(b"2026-10-03T23:58:10z"), None); // lowercase z
        assert_eq!(parse_ts(b"2026-10-03t23:58:10Z"), None); // lowercase t
        assert_eq!(parse_ts(b"2026-10-03 23:58:10Z"), None); // space for T
        assert_eq!(parse_ts(b"2026-10-03T23:58:10"), None); // no zone
        assert_eq!(parse_ts(b"2026-10-03T23:58:10+00:00x"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10Z\n"), None);
    }

    #[test]
    fn calendar_validation_matches_the_oracle() {
        assert_eq!(parse_ts(b"2028-02-29T00:00:00Z"), Some(1_835_395_200_000));
        assert_eq!(parse_ts(b"2027-02-29T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2028-02-30T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-00-01T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-01-00T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-04-31T00:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-10-03T24:00:00Z"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:60:00Z"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:60Z"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10+24:00"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10+05:60"), None);
        assert_eq!(parse_ts(b"2026-10-03T23:58:10+0200"), None);
    }

    #[test]
    fn year_zero_is_a_gregorian_leap_year() {
        assert_eq!(parse_ts(b"0000-01-01T00:00:00Z"), Some(-62_167_219_200_000));
        assert_eq!(parse_ts(b"0000-02-29T00:00:00Z"), Some(-62_162_121_600_000));
        assert_eq!(parse_ts(b"0000-03-01T00:00:00Z"), Some(-62_162_035_200_000));
        assert_eq!(parse_ts(b"0000-02-30T00:00:00Z"), None);
    }

    #[test]
    fn offsets_subtract_and_allow_boundary_values() {
        let utc = 1_791_071_890_000;
        assert_eq!(
            parse_ts(b"2026-10-03T23:58:10+23:59"),
            Some(utc - (23 * 3_600_000 + 59 * 60_000))
        );
        assert_eq!(
            parse_ts(b"2026-10-03T23:58:10-23:59"),
            Some(utc + (23 * 3_600_000 + 59 * 60_000))
        );
        assert_eq!(parse_ts(b"2026-10-03T23:58:10-00:00"), Some(utc));
        assert_eq!(parse_ts(b"2026-10-03T23:58:10+00:00"), Some(utc));
    }

    #[test]
    fn offset_can_change_the_utc_date() {
        // E7: 01:30 at +02:00 is 23:30Z on the previous day.
        let ts = parse_ts(b"2026-10-04T01:30:00+02:00");
        assert_eq!(ts, parse_ts(b"2026-10-03T23:30:00Z"));
        let ts = ts.unwrap();
        assert_eq!(
            day_of(ts),
            day_of(parse_ts(b"2026-10-03T12:00:00Z").unwrap())
        );
    }

    #[test]
    fn day_of_floors_towards_negative_infinity() {
        assert_eq!(day_of(0), 0);
        assert_eq!(day_of(-1), -1);
        assert_eq!(day_of(-86_399_999), -1);
        assert_eq!(day_of(-86_400_000), -1);
        assert_eq!(day_of(-86_400_001), -2);
        assert_eq!(day_of(DAY_MS), 1);
        assert_eq!(day_of(-1000), -1); // 1969-12-31T23:59:59Z
    }

    #[test]
    fn day_of_is_exact_on_its_documented_domain_and_saturates_outside() {
        let min_day = i32::MIN as i64 * DAY_MS;
        let max_day = i32::MAX as i64 * DAY_MS + (DAY_MS - 1);
        assert_eq!(day_of(min_day), i32::MIN);
        assert_eq!(day_of(max_day), i32::MAX);
        assert_eq!(day_of(min_day - 1), i32::MIN);
        assert_eq!(day_of(max_day + 1), i32::MAX);
        assert_eq!(day_of(i64::MIN), i32::MIN);
        assert_eq!(day_of(i64::MAX), i32::MAX);
    }

    const FIXTURE_MANIFEST: &str = "{\n  \"window_start\": \"2026-10-01T00:00:00Z\",\n  \"window_end\": \"2026-10-15T00:00:00Z\"\n}\n";
    const MINIFIED_MANIFEST: &str =
        r#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z"}"#;
    const DEV_MANIFEST: &str = "{\n  \"window_start\": \"2027-12-09T00:00:00Z\",\n  \"window_end\": \"2027-12-23T00:00:00Z\"\n}\n";

    #[test]
    fn window_parses_fixture_and_dev_manifests() {
        let expected = Window {
            start: 1_790_812_800_000,
            end: 1_792_022_400_000,
        };
        assert_eq!(Window::parse(FIXTURE_MANIFEST.as_bytes()), Some(expected));
        assert_eq!(Window::parse(MINIFIED_MANIFEST.as_bytes()), Some(expected));

        let dev = Window::parse(DEV_MANIFEST.as_bytes()).unwrap();
        assert_eq!(
            dev,
            Window {
                start: parse_ts(b"2027-12-09T00:00:00Z").unwrap(),
                end: parse_ts(b"2027-12-23T00:00:00Z").unwrap(),
            }
        );
    }

    #[test]
    fn window_allows_whitespace_and_any_key_order() {
        let input = " \r\n\t { \n\t\"window_end\" :\t\"2026-10-15T00:00:00Z\" ,\r\n\
                     \"window_start\"\n:\r\"2026-10-01T00:00:00Z\" } \n";
        let window = Window::parse(input.as_bytes()).unwrap();
        assert_eq!(window.start, 1_790_812_800_000);
        assert_eq!(window.end, 1_792_022_400_000);

        // Unknown string-valued keys are ignored.
        let input = r#"{"note": "x", "window_start": "2026-10-01T00:00:00Z",
                        "window_end": "2026-10-15T00:00:00Z"}"#;
        assert!(Window::parse(input.as_bytes()).is_some());
    }

    #[test]
    fn window_rejects_missing_or_bad_bounds() {
        // Missing bounds.
        assert_eq!(
            Window::parse(br#"{"window_start": "2026-10-01T00:00:00Z"}"#),
            None
        );
        assert_eq!(
            Window::parse(br#"{"window_end": "2026-10-15T00:00:00Z"}"#),
            None
        );
        assert_eq!(Window::parse(b"{}"), None);
        // Duplicate bounds.
        assert_eq!(
            Window::parse(
                br#"{"window_start": "2026-10-01T00:00:00Z",
                     "window_end": "2026-10-15T00:00:00Z",
                     "window_start": "2026-10-02T00:00:00Z"}"#
            ),
            None
        );
        // Bad timestamp values.
        for bad in [
            "2026-10-01T00:00:00",  // no zone
            "2026-13-01T00:00:00Z", // month 13
            "not-a-timestamp",      // garbage
            "",                     // empty
        ] {
            let manifest =
                format!(r#"{{"window_start": "{bad}", "window_end": "2026-10-15T00:00:00Z"}}"#);
            assert_eq!(Window::parse(manifest.as_bytes()), None, "{bad:?}");
        }
        // Non-string values and malformed JSON.
        assert_eq!(
            Window::parse(
                br#"{"window_start": 1790812800000, "window_end": "2026-10-15T00:00:00Z"}"#
            ),
            None
        );
        assert_eq!(
            Window::parse(FIXTURE_MANIFEST.trim_end().as_bytes()),
            Some(Window {
                start: 1_790_812_800_000,
                end: 1_792_022_400_000,
            })
        );
        assert_eq!(
            Window::parse(br#"{"window_start": "2026-10-01T00:00:00Z" "#),
            None
        );
        assert_eq!(
            Window::parse(
                br#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z"}x"#
            ),
            None
        );
        assert_eq!(
            Window::parse(
                br#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z",}"#
            ),
            None
        );
        assert_eq!(
            Window::parse(
                br#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z\u0041"}"#
            ),
            None
        );
    }
}
