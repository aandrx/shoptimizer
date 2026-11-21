//! V1 single-threaded pipeline.
//!
//! Implements `rules.md` "Events, in this order", "Clicks" and "Outputs"
//! literally, in the same order as `reference/reference.py`:
//!
//! events -> dedup -> validate -> late -> people -> sessions -> credit/assists
//! -> aggregate -> write.
//!
//! Input bytes stay in memory and every ID, timestamp source and category is a
//! borrowed slice of that input, so a row costs no per-event allocation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;

use crate::agg::{Aggregates, ORGANIC};
use crate::csv;
use crate::people::People;
use crate::ts::{self, Window};

const SESSION_GAP_MS: i64 = 30 * 60 * 1_000;
const LATE_MS: i64 = 24 * 60 * 60 * 1_000;
const CLICK_LOOKBACK_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_PRICE: i64 = 10_000_000;

/// rules.md step 7: a product missing from `products.csv` has this category.
const UNKNOWN: &[u8] = b"unknown";

fn phase(name: &str, started: Instant) {
    eprintln!("[phase] {name} {}ms", started.elapsed().as_millis());
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Every `*.csv` file in `dir`, sorted by path (the reference's `sorted(glob)`).
fn csv_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "csv") && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_dir_csvs(dir: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut buffers = Vec::new();
    for path in csv_files(dir)? {
        buffers.push(fs::read(&path)?);
    }
    Ok(buffers)
}

/// The whole input, read once and borrowed from for the rest of the run.
struct Inputs {
    manifest: Vec<u8>,
    products: Vec<u8>,
    campaigns: Vec<u8>,
    events: Vec<Vec<u8>>,
    clicks: Vec<Vec<u8>>,
    links: Vec<Vec<u8>>,
}

fn read_inputs(data: &Path) -> io::Result<Inputs> {
    Ok(Inputs {
        manifest: fs::read(data.join("manifest.json"))?,
        products: fs::read(data.join("products.csv"))?,
        campaigns: fs::read(data.join("campaigns.csv"))?,
        events: read_dir_csvs(&data.join("events"))?,
        clicks: read_dir_csvs(&data.join("clicks"))?,
        links: read_dir_csvs(&data.join("links"))?,
    })
}

/// `product_id` -> `category`, later lines winning (the reference's dict).
fn lookup_table(buf: &[u8]) -> HashMap<&[u8], &[u8]> {
    let mut table = HashMap::new();
    for line in csv::data_lines(buf) {
        if let Some([Some(key), Some(value)]) = csv::split::<2>(line) {
            table.insert(key, value);
        }
    }
    table
}

/// rules.md "Events" step 3: digits only, value 1..=10,000,000. `0` = invalid.
///
/// Leading zeros are fine (`0250` is 250). A value that would overflow the
/// running total is over `MAX_PRICE` anyway, so it is invalid too; nothing wraps.
fn parse_price(raw: &[u8]) -> i64 {
    if raw.is_empty() {
        return 0;
    }
    let mut value: i64 = 0;
    for &byte in raw {
        if !byte.is_ascii_digit() {
            return 0;
        }
        value = match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(byte - b'0')))
        {
            Some(v) if v <= MAX_PRICE => v,
            _ => return 0,
        };
    }
    value
}

/// The kept copy of an event, with the expensive parses already done. Dedup runs
/// before validation, so an invalid copy is kept when it is the earliest one and
/// then dropped; a later valid copy never rescues it.
struct EventRecord<'a> {
    /// Epoch ms, `i64::MAX` when `ingest_ts` is not a valid F1 (never valid, but
    /// matches the reference's `+inf` sentinel).
    ingest: i64,
    user: Option<&'a [u8]>,
    ts: Option<i64>,
    /// 0 = not view/cart/purchase, 1 = view, 2 = cart, 3 = purchase.
    kind: u8,
    product: Option<&'a [u8]>,
    /// Purchase price; 0 = invalid or not a purchase.
    price: i64,
}

fn kind_of(raw: Option<&[u8]>) -> u8 {
    match raw {
        Some(kind) if kind == b"view" => 1,
        Some(kind) if kind == b"cart" => 2,
        Some(kind) if kind == b"purchase" => 3,
        _ => 0,
    }
}

/// A surviving, attributed event. Only what sessions and aggregates need.
struct Event<'a> {
    ts: i64,
    purchase: bool,
    price: i64,
    category: &'a [u8],
}

/// A valid click. Sorted by `(ts, click_id)` per person.
struct Click<'a> {
    ts: i64,
    click_id: &'a [u8],
    campaign: &'a [u8],
}

fn bump<'a>(counts: &mut HashMap<&'a [u8], u32>, campaign: &'a [u8]) {
    *counts.entry(campaign).or_insert(0) += 1;
}

fn unbump<'a>(counts: &mut HashMap<&'a [u8], u32>, campaign: &'a [u8]) {
    if let Entry::Occupied(mut entry) = counts.entry(campaign) {
        let count = entry.get_mut();
        *count -= 1;
        if *count == 0 {
            entry.remove();
        }
    }
}

/// events -> dedup -> validate -> late -> people -> sessions -> credit/assists
/// -> aggregate -> write.
pub fn run(data: &Path, out: &Path) -> io::Result<()> {
    let started = Instant::now();
    let Inputs {
        manifest,
        products,
        campaigns,
        events,
        clicks,
        links,
    } = read_inputs(data)?;
    phase("read_inputs", started);

    let started = Instant::now();
    let window = Window::parse(&manifest).ok_or_else(|| invalid("manifest.json"))?;
    let products = lookup_table(&products);
    let campaigns: HashMap<&[u8], &[u8]> = lookup_table(&campaigns);
    let people = People::build(links.iter().flat_map(|buf| {
        csv::data_lines(buf).filter_map(|line| {
            let [a, b] = csv::split::<2>(line)?;
            Some((a?, b?))
        })
    }));
    phase("lookups", started);

    // ---- Events: parse, dedup on event_id (smallest ingest_ts), validate, late
    let started = Instant::now();
    let mut kept: HashMap<&[u8], EventRecord> = HashMap::new();
    for buf in &events {
        for line in csv::data_lines(buf) {
            let Some([event_id, user, ts_raw, ingest_raw, kind, product, price_raw]) =
                csv::split::<7>(line)
            else {
                continue;
            };
            // rules.md "Events" step 1: an empty event_id drops the line.
            let Some(event_id) = event_id else {
                continue;
            };
            let kind = kind_of(kind);
            let record = EventRecord {
                ingest: ingest_raw.and_then(ts::parse_ts).unwrap_or(i64::MAX),
                user,
                ts: ts_raw.and_then(ts::parse_ts),
                kind,
                product,
                price: if kind == 3 {
                    price_raw.map(parse_price).unwrap_or(0)
                } else {
                    0
                },
            };
            match kept.entry(event_id) {
                Entry::Occupied(mut entry) => {
                    // Equal ingest_ts copies are identical: either is fine.
                    if record.ingest < entry.get().ingest {
                        entry.insert(record);
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(record);
                }
            }
        }
    }
    phase("dedup", started);

    let started = Instant::now();
    let mut by_person: HashMap<&[u8], Vec<Event>> = HashMap::new();
    for (_, record) in kept {
        let (Some(user), Some(ts), Some(product)) = (record.user, record.ts, record.product) else {
            continue;
        };
        if record.kind == 0 || !(window.start..window.end).contains(&ts) {
            continue;
        }
        let purchase = record.kind == 3;
        // For view/cart the price is ignored whatever it holds.
        if purchase && record.price == 0 {
            continue;
        }
        // rules.md "Events" step 4: over 24 h late is dropped; equal stays and an
        // ingest before ts stays. `i64::MAX` (bad ingest) behaves like `+inf`.
        if record.ingest.saturating_sub(ts) > LATE_MS {
            continue;
        }
        let category = products.get(product).copied().unwrap_or(UNKNOWN);
        let who = people.name(user);
        by_person.entry(who).or_default().push(Event {
            ts,
            purchase,
            price: record.price,
            category,
        });
    }
    phase("validate", started);

    // ---- Clicks: parse, validate. No dedup (D7): copies are identical, and
    // credit/assists only need the presence of a click, so duplicates change
    // nothing. Every row is still validated on its own.
    let started = Instant::now();
    let click_start = window.start.saturating_sub(CLICK_LOOKBACK_MS);
    let mut clicks_by_person: HashMap<&[u8], Vec<Click>> = HashMap::new();
    for buf in &clicks {
        for line in csv::data_lines(buf) {
            let Some([click_id, user, campaign, ts_raw]) = csv::split::<4>(line) else {
                continue;
            };
            // rules.md "Clicks" step 1: an empty click_id drops the line.
            let Some(click_id) = click_id else {
                continue;
            };
            let (Some(user), Some(campaign)) = (user, campaign) else {
                continue;
            };
            if !campaigns.contains_key(campaign) {
                continue;
            }
            let Some(ts) = ts_raw.and_then(ts::parse_ts) else {
                continue;
            };
            if ts < click_start || ts >= window.end {
                continue;
            }
            let who = people.name(user);
            clicks_by_person.entry(who).or_default().push(Click {
                ts,
                click_id,
                campaign,
            });
        }
    }
    for list in clicks_by_person.values_mut() {
        // Latest ts wins; the smallest click_id breaks a tie, so the first click
        // of the latest-ts group is the credited one.
        list.sort_by(|a, b| (a.ts, a.click_id).cmp(&(b.ts, b.click_id)));
    }
    phase("clicks", started);

    // ---- Sessions, attribution, assists, aggregation
    let started = Instant::now();
    let mut aggregates = Aggregates::default();
    for (who, mut events) in by_person {
        events.sort_by_key(|event| event.ts);
        let clicks: &[Click] = clicks_by_person.get(who).map(Vec::as_slice).unwrap_or(&[]);
        // The click window [start - 7d, start] slides forward with the session
        // starts, so each click enters and leaves the campaign counts once.
        let mut lo = 0;
        let mut hi = 0;
        let mut active: HashMap<&[u8], u32> = HashMap::new();
        let mut session_start = 0;
        while session_start < events.len() {
            let start = events[session_start].ts;
            let mut session_end = session_start + 1;
            let mut previous = start;
            // The gap is measured between consecutive distinct timestamps, so
            // equal timestamps share a session whatever order they were read in.
            while session_end < events.len() && events[session_end].ts - previous <= SESSION_GAP_MS
            {
                previous = events[session_end].ts;
                session_end += 1;
            }
            let session = &events[session_start..session_end];

            let lower = start.saturating_sub(CLICK_LOOKBACK_MS);
            // Add every click up to the session start first, so the removal
            // below also sees clicks this session just brought into range.
            while hi < clicks.len() && clicks[hi].ts <= start {
                bump(&mut active, clicks[hi].campaign);
                hi += 1;
            }
            while lo < hi && clicks[lo].ts < lower {
                unbump(&mut active, clicks[lo].campaign);
                lo += 1;
            }
            let credit = if hi > lo {
                let latest = clicks[hi - 1].ts;
                let first = lo + clicks[lo..hi].partition_point(|click| click.ts < latest);
                let campaign = clicks[first].campaign;
                (
                    campaign,
                    campaigns.get(campaign).copied().unwrap_or(ORGANIC),
                )
            } else {
                (ORGANIC, ORGANIC)
            };
            let assists = active.iter().map(|(&campaign, _)| {
                (
                    campaign,
                    campaigns.get(campaign).copied().unwrap_or(ORGANIC),
                )
            });
            let purchases = session
                .iter()
                .filter(|event| event.purchase)
                .map(|event| (event.category, event.price));
            aggregates.add_session(
                who,
                ts::day_of(start),
                session.len() as i64,
                purchases,
                credit,
                assists,
            );
            session_start = session_end;
        }
    }
    phase("sessions", started);

    let started = Instant::now();
    aggregates.write(out)?;
    phase("write", started);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::fs::File;

    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;

    #[test]
    fn price_accepts_only_one_to_ten_million() {
        assert_eq!(parse_price(b"250"), 250);
        assert_eq!(parse_price(b"0250"), 250);
        assert_eq!(parse_price(b"1"), 1);
        assert_eq!(parse_price(b"10000000"), MAX_PRICE);
        assert_eq!(parse_price(b"10000001"), 0);
        assert_eq!(parse_price(b"999999999999999999999999"), 0);
    }

    #[test]
    fn price_rejects_non_digits_and_empty() {
        assert_eq!(parse_price(b""), 0);
        assert_eq!(parse_price(b"0"), 0);
        assert_eq!(parse_price(b"000"), 0);
        assert_eq!(parse_price(b"1000\r"), 0);
        assert_eq!(parse_price(b"-5"), 0);
        assert_eq!(parse_price(b"+5"), 0);
        assert_eq!(parse_price(b"1.0"), 0);
        assert_eq!(parse_price(b" 1"), 0);
        assert_eq!(parse_price(b"\xd9\xa5"), 0);
    }

    #[test]
    fn kinds_are_case_and_space_sensitive() {
        assert_eq!(kind_of(Some(b"view")), 1);
        assert_eq!(kind_of(Some(b"cart")), 2);
        assert_eq!(kind_of(Some(b"purchase")), 3);
        assert_eq!(kind_of(Some(b"Purchase")), 0);
        assert_eq!(kind_of(Some(b"cart ")), 0);
        assert_eq!(kind_of(Some(b"")), 0);
        assert_eq!(kind_of(None), 0);
    }

    /// `(campaign_id, channel, day, sessions, purchasing, revenue, assisted)`.
    type CampaignRow = (String, String, i32, i64, i64, i64, i64);

    fn string(field: &Field) -> String {
        match field {
            Field::Str(value) => value.clone(),
            Field::Bytes(value) => String::from_utf8(value.data().to_vec()).unwrap(),
            other => panic!("expected a string, got {other:?}"),
        }
    }

    fn integer(field: &Field) -> i64 {
        match field {
            Field::Date(value) | Field::Int(value) => i64::from(*value),
            Field::Long(value) => *value,
            other => panic!("expected an integer, got {other:?}"),
        }
    }

    fn campaign_rows(out: &Path) -> Vec<CampaignRow> {
        let file = File::open(out.join("campaign_daily").join("part-0.parquet")).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        reader
            .get_row_iter(None)
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                let fields: Vec<&Field> = row.get_column_iter().map(|(_, field)| field).collect();
                (
                    string(fields[0]),
                    string(fields[1]),
                    integer(fields[2]) as i32,
                    integer(fields[3]),
                    integer(fields[4]),
                    integer(fields[5]),
                    integer(fields[6]),
                )
            })
            .collect()
    }

    /// D7: click copies are not deduped. They must still credit and assist the
    /// session once, with no extra session or assist count.
    #[test]
    fn duplicate_click_copies_credit_and_assist_once() {
        let dir =
            std::env::temp_dir().join(format!("shoptimizer-t05-dupclick-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let data = dir.join("data");
        let out = dir.join("out");
        for folder in ["events", "clicks", "links"] {
            fs::create_dir_all(data.join(folder)).unwrap();
        }
        fs::write(
            data.join("manifest.json"),
            r#"{"window_start": "2026-10-01T00:00:00Z", "window_end": "2026-10-15T00:00:00Z"}"#,
        )
        .unwrap();
        fs::write(data.join("products.csv"), "product_id,category\np1,shoes\n").unwrap();
        fs::write(
            data.join("campaigns.csv"),
            "campaign_id,channel\nc_1,search\n",
        )
        .unwrap();
        fs::write(data.join("links/part-00000.csv"), "user_id_a,user_id_b\n").unwrap();
        fs::write(
            data.join("events/part-00000.csv"),
            "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents\n\
             e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,view,p1,\n",
        )
        .unwrap();
        // The same click twice, quoted differently: identical values.
        fs::write(
            data.join("clicks/part-00000.csv"),
            "click_id,user_id,campaign_id,ts\n\
             k1,u1,c_1,2026-10-03T09:00:00Z\n\
             \"k1\",\"u1\",\"c_1\",\"2026-10-03T09:00:00Z\"\n",
        )
        .unwrap();

        run(&data, &out).unwrap();

        assert_eq!(
            campaign_rows(&out),
            vec![("c_1".to_string(), "search".to_string(), 20729, 1, 0, 0, 1)]
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
