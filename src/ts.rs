//! rules.md "Timestamps".

/// F1 / F2 / F3 -> epoch ms UTC. None = bad timestamp (never panics, incl. F3 overflow).
pub fn parse_ts(s: &[u8]) -> Option<i64> {
    let _ = s;
    todo!()
}

/// Days since 1970-01-01 for a UTC epoch-ms instant (floor division).
pub fn day_of(ms: i64) -> i32 {
    let _ = ms;
    todo!()
}

/// Observation window, from manifest.json.
pub struct Window {
    pub start: i64,
    pub end: i64,
}
