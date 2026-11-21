//! Single-threaded reference-quality pipeline:
//! events -> dedup -> validate -> late -> people -> sessions -> credit/assists -> aggregate.
//!
//! Every input file is read fully into memory and kept alive for the whole run, so IDs are
//! `&[u8]` slices into those buffers.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::agg::{Aggregator, SessionSummary};
use crate::csv::{data_lines, split};
use crate::output::{write_campaign_daily, write_user_daily};
use crate::people::People;
use crate::ts::{day_of, parse_manifest, parse_ts};

const SESSION_GAP: i64 = 1_800_000;
const LATE: i64 = 86_400_000;
const LOOKBACK: i64 = 604_800_000;
const MAX_PRICE: i64 = 10_000_000;

fn bad_data(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Logs `[phase] <name> <ms>` and restarts the timer.
fn phase(t: &mut Instant, name: &str) {
    eprintln!("[phase] {} {}", name, t.elapsed().as_millis());
    *t = Instant::now();
}

/// Every `*.csv` file of `dir`, read fully, in sorted path order. A missing dir reads as empty.
fn read_dir_csv(dir: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "csv") && p.is_file())
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    paths.sort();
    paths.iter().map(fs::read).collect()
}

/// `id -> value` from a clean two-column file (products.csv, campaigns.csv).
fn lookup(buf: &[u8]) -> HashMap<&[u8], &[u8]> {
    let mut m = HashMap::new();
    for line in data_lines(buf) {
        if let Some([Some(k), Some(v)]) = split::<2>(line) {
            m.insert(k, v);
        }
    }
    m
}

/// ASCII digits only, value 1..=MAX_PRICE.
fn parse_price(s: &[u8]) -> Option<i64> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: i64 = 0;
    for &c in s {
        v = v * 10 + (c - b'0') as i64;
        if v > MAX_PRICE {
            return None;
        }
    }
    (v >= 1).then_some(v)
}

/// One surviving event of a person.
struct Ev<'a> {
    ts: i64,
    /// Purchase price in cents; 0 for view and cart.
    price: i64,
    purchase: bool,
    category: &'a [u8],
}

/// One valid click of a person.
struct Click<'a> {
    ts: i64,
    id: &'a [u8],
    campaign: &'a [u8],
}

pub fn run(data: &Path, out: &Path) -> io::Result<()> {
    let mut t = Instant::now();

    // ---- Read everything.
    let manifest = fs::read(data.join("manifest.json"))?;
    let window = parse_manifest(&manifest).ok_or_else(|| bad_data("bad manifest.json".into()))?;
    let click_start = window.start - LOOKBACK;
    let products_buf = fs::read(data.join("products.csv"))?;
    let campaigns_buf = fs::read(data.join("campaigns.csv"))?;
    let link_bufs = read_dir_csv(&data.join("links"))?;
    let event_bufs = read_dir_csv(&data.join("events"))?;
    let click_bufs = read_dir_csv(&data.join("clicks"))?;
    phase(&mut t, "read");

    let products = lookup(&products_buf);
    let campaigns = lookup(&campaigns_buf);

    // ---- Links -> people.
    let people = People::build(link_bufs.iter().flat_map(|b| data_lines(b)).filter_map(|line| {
        match split::<2>(line)? {
            [Some(a), Some(b)] => Some((a, b)),
            _ => None,
        }
    }));
    phase(&mut t, "links");

    // ---- Events: parse, dedup on event_id keeping the smallest ingest_ts.
    // Value: (ingest_ms, line of the kept copy; re-split later to keep the map small).
    // Ties keep the first copy (copies with equal ingest_ts are identical).
    let mut kept: HashMap<&[u8], (i64, &[u8])> = HashMap::new();
    for line in event_bufs.iter().flat_map(|b| data_lines(b)) {
        let Some(f) = split::<7>(line) else { continue };
        let Some(event_id) = f[0] else { continue };
        // rules.md: ingest_ts is always valid F1; a bad one never wins a tie.
        let ingest = f[3].and_then(parse_ts).unwrap_or(i64::MAX);
        kept.entry(event_id)
            .and_modify(|e| {
                if ingest < e.0 {
                    *e = (ingest, line);
                }
            })
            .or_insert((ingest, line));
    }
    phase(&mut t, "events_dedup");

    // ---- Validate the kept copy, drop late, map to person.
    let mut events_by_person: HashMap<&[u8], Vec<Ev>> = HashMap::new();
    for (_, (ingest, line)) in kept.drain() {
        let Some(f) = split::<7>(line) else { unreachable!("kept lines parsed once already") };
        let [_, Some(user), Some(ts_raw), _, Some(event_type), Some(product), price_raw] = f else {
            continue;
        };
        let Some(ts) = parse_ts(ts_raw) else { continue };
        if ts < window.start || ts >= window.end {
            continue;
        }
        let (purchase, price) = match event_type {
            b"view" | b"cart" => (false, 0),
            b"purchase" => match price_raw.and_then(parse_price) {
                Some(p) => (true, p),
                None => continue,
            },
            _ => continue,
        };
        // ingest is i64::MAX only for a bad ingest_ts; saturate so it counts as late.
        if ingest.saturating_sub(ts) > LATE {
            continue;
        }
        let category: &[u8] = products.get(product).copied().unwrap_or(b"unknown");
        events_by_person.entry(people.name(user)).or_default().push(Ev { ts, price, purchase, category });
    }
    drop(kept);
    phase(&mut t, "events_validate");

    // ---- Clicks: parse, validate, map to person (no dedup: copies are identical, PLAN D7).
    let mut clicks_by_person: HashMap<&[u8], Vec<Click>> = HashMap::new();
    for line in click_bufs.iter().flat_map(|b| data_lines(b)) {
        let Some([Some(id), Some(user), Some(campaign), Some(ts_raw)]) = split::<4>(line) else { continue };
        if !campaigns.contains_key(campaign) {
            continue;
        }
        let Some(ts) = parse_ts(ts_raw) else { continue };
        if ts < click_start || ts >= window.end {
            continue;
        }
        clicks_by_person.entry(people.name(user)).or_default().push(Click { ts, id, campaign });
    }
    phase(&mut t, "clicks");

    // ---- Sessionize, credit, assists, aggregate.
    let mut agg = Aggregator::new();
    let mut active: HashMap<&[u8], u32> = HashMap::new();
    for (person, evs) in events_by_person.iter_mut() {
        evs.sort_unstable_by_key(|e| e.ts);
        let clicks: &mut [Click] = clicks_by_person.get_mut(person).map_or(&mut [], |v| v.as_mut_slice());
        clicks.sort_unstable_by(|a, b| (a.ts, a.id).cmp(&(b.ts, b.id)));
        let clicks: &[Click] = clicks;

        // Sliding click window [lo, hi): click.ts in [start - LOOKBACK, start].
        // `active` counts the window's clicks per campaign.
        active.clear();
        let (mut lo, mut hi) = (0usize, 0usize);
        let mut i = 0;
        while i < evs.len() {
            let start = evs[i].ts;
            let mut j = i + 1;
            while j < evs.len() && evs[j].ts - evs[j - 1].ts <= SESSION_GAP {
                j += 1;
            }
            let session = &evs[i..j];
            i = j;

            while hi < clicks.len() && clicks[hi].ts <= start {
                *active.entry(clicks[hi].campaign).or_insert(0) += 1;
                hi += 1;
            }
            while lo < hi && clicks[lo].ts < start - LOOKBACK {
                let c = active.get_mut(clicks[lo].campaign).expect("active campaign");
                *c -= 1;
                if *c == 0 {
                    active.remove(clicks[lo].campaign);
                }
                lo += 1;
            }

            let credited: &[u8] = if lo < hi {
                // Latest ts wins; among equal ts the smallest click_id, which is the first of
                // that ts run in (ts, click_id) order.
                let latest = clicks[hi - 1].ts;
                let mut k = hi - 1;
                while k > lo && clicks[k - 1].ts == latest {
                    k -= 1;
                }
                clicks[k].campaign
            } else {
                b"organic"
            };

            agg.add_session(&SessionSummary {
                person: *person,
                day: day_of(start),
                events: session.len() as i64,
                purchases: session.iter().filter(|e| e.purchase).map(|e| (e.category, e.price)).collect(),
                credited,
                assists: active.keys().copied().collect(),
            });
        }
    }
    phase(&mut t, "sessions");

    let user_rows = agg.user_daily();
    let campaign_rows = agg.campaign_daily(|c| campaigns.get(c).copied().unwrap_or(b"unknown"));
    write_user_daily(&out.join("user_daily"), &user_rows)?;
    write_campaign_daily(&out.join("campaign_daily"), &campaign_rows)?;
    phase(&mut t, "write");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price() {
        assert_eq!(parse_price(b"1"), Some(1));
        assert_eq!(parse_price(b"0250"), Some(250));
        assert_eq!(parse_price(b"10000000"), Some(10_000_000));
        assert_eq!(parse_price(b"000000000000000010000000"), Some(10_000_000));
        for bad in [&b""[..], b"0", b"10000001", b"19.99", b"-5", b"+5", b"1000\r", b" 1", "２５０".as_bytes(), b"99999999999999999999999"] {
            assert_eq!(parse_price(bad), None, "{:?}", bad);
        }
    }
}
