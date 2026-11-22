//! V2 parallel pipeline (PLAN.md §6, Submission #2).
//!
//! P0 lookups -> P1 parse + scatter -> P2 dedup + re-partition by person ->
//! P3 sessions -> P4 write. Falls back to [`crate::v1::run`] whenever the input
//! doesn't fit V2's compact encoding (D6, D13).

pub mod parse;
pub mod people;
pub mod scan;
pub mod sessions;
pub mod split;
pub mod types;

mod dedup;

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;

use crate::agg::ORGANIC;
use crate::id;
use crate::output::{self, CampaignDaily, UserDaily};
use crate::ts::Window;

use sessions::{Acc, ASSISTED, PURCHASING, REVENUE, SESSIONS};
use types::{Ck, Ctx, Ev, Parsed};

/// Phase log line; `SHOPT_IO_STATS=1` adds fault and RSS fields (S3-7).
fn phase(name: &str, started: Instant) {
    crate::iostats::log_phase(name, started);
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Widest bucket width either knob may request (same bound as the legacy knob).
const MAX_BITS: u32 = 16;

/// Upper bound on `nb_dedup * nb_person`, the `Flat` offset-cell count (S4-2).
///
/// At the defaults (8, 8) this is 65_536; the largest sensible tuning point is
/// far below the cap. A larger request is rejected with a V1 fallback before
/// any per-bucket allocation, never satisfied by dropping or clamping buckets.
const MAX_BUCKET_CELLS: usize = 1 << 22;

/// Parse one bits knob: absent or unparseable means unset; a value above
/// [`MAX_BITS`] is bounded exactly like the legacy `SHOPT_BUCKET_BITS`.
fn parse_bits(raw: Option<&str>) -> Option<u32> {
    let v: usize = raw?.parse().ok()?;
    Some(v.min(MAX_BITS as usize) as u32)
}

/// Resolve `(dedup_bits, person_bits)` (S4-2).
///
/// `SHOPT_DEDUP_BITS` and `SHOPT_PERSON_BITS` each override their own side of
/// the legacy `SHOPT_BUCKET_BITS` shorthand, which still sets both sides when
/// it is the only knob set. Both sides default to 8 bits.
fn resolve_bits(shorthand: Option<&str>, dedup: Option<&str>, person: Option<&str>) -> (u32, u32) {
    let legacy = parse_bits(shorthand);
    (
        parse_bits(dedup).or(legacy).unwrap_or(8),
        parse_bits(person).or(legacy).unwrap_or(8),
    )
}

/// The two bucket widths from the environment, in `(dedup, person)` order.
fn bucket_bits() -> (u32, u32) {
    let shorthand = std::env::var("SHOPT_BUCKET_BITS").ok();
    let dedup = std::env::var("SHOPT_DEDUP_BITS").ok();
    let person = std::env::var("SHOPT_PERSON_BITS").ok();
    resolve_bits(shorthand.as_deref(), dedup.as_deref(), person.as_deref())
}

/// Initial capacity for one worker's bucket `Vec` (S3-5):
/// `total_bytes / bytes_per_row / workers / nb * 1.15`.
///
/// Only a hint: under-estimates grow normally, so correctness never depends on
/// it. `bytes_per_row` is the observed average row size (events 70, clicks 45).
fn prealloc_cap(total_bytes: usize, bytes_per_row: usize, workers: usize, nb: usize) -> usize {
    let rows = total_bytes / bytes_per_row.max(1);
    (rows as u64 * 115 / (100 * workers.max(1) as u64 * nb.max(1) as u64)) as usize
}

/// Total record count per bucket across every worker's bucket set.
///
/// Computed once per bucket set (S3-6) so P3 neither rescans the parts per
/// bucket nor pays for it twice.
fn bucket_lens<T: Sync>(parts: &[Vec<Vec<T>>], nb: usize) -> Vec<usize> {
    (0..nb)
        .into_par_iter()
        .map(|b| parts.iter().map(|p| p[b].len()).sum())
        .collect()
}

/// Total `Ev` count per person bucket across the flat P2 outputs (S4-1).
fn flat_lens(parts: &[dedup::Flat], nb: usize) -> Vec<usize> {
    (0..nb)
        .into_par_iter()
        .map(|b| parts.iter().map(|p| p.bucket(b).len()).sum())
        .collect()
}

/// P3 schedule (S3-6): buckets in descending order of their total `Ev` count so
/// the whale bucket starts first instead of finishing last. Ties keep bucket
/// index order, making the schedule deterministic.
fn bucket_order(ev_lens: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ev_lens.len()).collect();
    order.sort_unstable_by(|&a, &b| ev_lens[b].cmp(&ev_lens[a]).then(a.cmp(&b)));
    order
}

/// P1 scatter: exactly one bucket set per worker, chunks pulled from a shared
/// index (D14, D9).
///
/// Spawns `rayon::current_num_threads()` tasks into the current pool; no nested
/// pool, so a fixed pool size is never oversubscribed. Each task owns its own
/// `Vec<Vec<T>>` and takes the next unclaimed chunk until none are left, which
/// keeps large chunks from stranding a worker.
fn scatter<T, F>(
    chunks: &[&[u8]],
    nb: usize,
    cap: usize,
    overflow: &AtomicBool,
    parse: F,
) -> Vec<Vec<Vec<T>>>
where
    T: Send,
    F: Fn(&[u8]) -> Parsed<(usize, T)> + Sync,
{
    let workers = rayon::current_num_threads().max(1);
    let next = AtomicUsize::new(0);
    let parts: Mutex<Vec<Vec<Vec<T>>>> = Mutex::new(Vec::with_capacity(workers));
    rayon::scope(|s| {
        for _ in 0..workers {
            s.spawn(|_| {
                let mut buckets: Vec<Vec<T>> = (0..nb).map(|_| Vec::with_capacity(cap)).collect();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(chunk) = chunks.get(i) else { break };
                    for line in scan::lines(chunk) {
                        match parse(line) {
                            Parsed::Rec((b, r)) => buckets[b].push(r),
                            Parsed::Skip => {}
                            Parsed::Overflow => overflow.store(true, Ordering::Relaxed),
                        }
                    }
                }
                parts.lock().unwrap().push(buckets);
            });
        }
    });
    parts.into_inner().unwrap()
}

/// Runs V2 and drops everything normally when it returns.
///
/// This is the entry point for tests and any caller that wants teardown to run;
/// the binary uses [`run_with`] instead.
#[allow(dead_code)]
pub fn run(data: &Path, out: &Path) -> io::Result<()> {
    run_with(data, out, || ())
}

/// Runs V2 and calls `on_success` once both Parquet writers have returned (files
/// closed), while this frame still owns the pipeline's heavy state (`acc`, the
/// mmaps, the maps). That lets the binary exit the process without paying for a
/// teardown it doesn't need.
///
/// `on_success` is only called on a successful V2 run: errors and the V1
/// fallback keep normal drops, so no memory is deliberately leaked there.
pub fn run_with(data: &Path, out: &Path, on_success: impl FnOnce()) -> io::Result<()> {
    if run_v2(data, out, on_success)? {
        Ok(())
    } else {
        eprintln!("[v2] input doesn't fit the compact encoding; falling back to v1");
        crate::v1::run(data, out)
    }
}

/// `Ok(false)` = fall back to V1 (nothing has been written yet).
fn run_v2(data: &Path, out: &Path, on_success: impl FnOnce()) -> io::Result<bool> {
    let started = Instant::now();
    let manifest = fs::read(data.join("manifest.json"))?;
    let window = Window::parse(&manifest)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "manifest.json"))?;
    let products = fs::read(data.join("products.csv"))?;
    let campaigns = fs::read(data.join("campaigns.csv"))?;
    let Some(ctx) = Ctx::build(window, &products, &campaigns) else {
        return Ok(false);
    };
    let event_maps = scan::map_dir(&data.join("events"))?;
    let click_maps = scan::map_dir(&data.join("clicks"))?;
    let link_maps = scan::map_dir(&data.join("links"))?;
    let chunk = env_usize("SHOPT_CHUNK_BYTES", 32 << 20);
    // S4-2: the dedup width sizes the P1 event scatter and the P2 task count;
    // the person width sizes the P2 person output, the click scatter and P3.
    let (dedup_bits, person_bits) = bucket_bits();
    let nb_dedup = 1usize << dedup_bits;
    let nb_person = 1usize << person_bits;
    // Checked before any per-bucket allocation: an absurd product is not
    // representable here, so fall back to V1 instead of omitting buckets.
    if nb_dedup.saturating_mul(nb_person) > MAX_BUCKET_CELLS {
        eprintln!(
            "[v2] bucket product {nb_dedup}*{nb_person} exceeds {MAX_BUCKET_CELLS}; falling back to v1"
        );
        return Ok(false);
    }
    phase("open", started);

    // ---- P1: parse + scatter. Events don't need the person map, so the links
    // build (mostly serial union-find) runs alongside the event parse.
    let started = Instant::now();
    let overflow = AtomicBool::new(false);
    let link_chunks = scan::chunks(&link_maps, chunk);
    let ev_chunks = scan::chunks(&event_maps, chunk);
    let (people, ev_parts) = rayon::join(
        || people::build(&link_chunks),
        || {
            let workers = rayon::current_num_threads().max(1);
            let bytes: usize = ev_chunks.iter().map(|c| c.len()).sum();
            let cap = prealloc_cap(bytes, 70, workers, nb_dedup);
            scatter(&ev_chunks, nb_dedup, cap, &overflow, |line| {
                match parse::event(line, &ctx) {
                    Parsed::Rec(r) => Parsed::Rec((id::bucket(r.id, dedup_bits), r)),
                    Parsed::Skip => Parsed::Skip,
                    Parsed::Overflow => Parsed::Overflow,
                }
            })
        },
    );
    let Some(people) = people else {
        return Ok(false);
    };
    phase("links+events", started);
    let started = Instant::now();
    let ck_chunks = scan::chunks(&click_maps, chunk);
    let ck_parts: Vec<Vec<Vec<Ck>>> = {
        let workers = rayon::current_num_threads().max(1);
        let bytes: usize = ck_chunks.iter().map(|c| c.len()).sum();
        let cap = prealloc_cap(bytes, 45, workers, nb_person);
        scatter(&ck_chunks, nb_person, cap, &overflow, |line| {
            match parse::click(line, &ctx) {
                Parsed::Rec((user, mut ck)) => {
                    ck.person = people.person(user);
                    Parsed::Rec((id::bucket(ck.person, person_bits), ck))
                }
                Parsed::Skip => Parsed::Skip,
                Parsed::Overflow => Parsed::Overflow,
            }
        })
    };
    if overflow.load(Ordering::Relaxed) {
        return Ok(false);
    }
    phase("clicks", started);

    // ---- P2: dedup per event bucket, then re-partition by person. Each dedup
    // task returns one flat `Vec<Ev>` with disjoint per-person-bucket ranges
    // (S4-1), so P3 only has to concatenate slices.
    let started = Instant::now();
    let ev_out: Vec<dedup::Flat> = match (0..nb_dedup)
        .into_par_iter()
        .map(|b| dedup::partition_bucket(b, &ev_parts, &people, person_bits, nb_person))
        .collect::<Option<Vec<_>>>()
    {
        Some(out) => out,
        // Checked offsets overflow `usize`: not representable here, use V1.
        None => return Ok(false),
    };
    // S3-3: free the parse buffers in the background instead of on the critical path.
    std::thread::spawn(move || drop(ev_parts));
    phase("dedup", started);

    // ---- P3: sessions per person bucket
    let started = Instant::now();
    let ev_lens = flat_lens(&ev_out, nb_person);
    let ck_lens = bucket_lens(&ck_parts, nb_person);
    let acc = bucket_order(&ev_lens)
        .into_par_iter()
        .fold(
            || Acc::new(&ctx),
            |mut acc, b| {
                let mut evs: Vec<Ev> = Vec::with_capacity(ev_lens[b]);
                for f in &ev_out {
                    evs.extend_from_slice(f.bucket(b));
                }
                let mut cks: Vec<Ck> = Vec::with_capacity(ck_lens[b]);
                for p in &ck_parts {
                    cks.extend_from_slice(&p[b]);
                }
                acc.run_bucket(&mut evs, &mut cks, &ctx);
                acc
            },
        )
        .reduce_with(Acc::merge)
        .unwrap_or_else(|| Acc::new(&ctx));
    // S3-3: background frees while the Parquet writers run.
    std::thread::spawn(move || drop(ev_out));
    std::thread::spawn(move || drop(ck_parts));
    phase("sessions", started);

    // ---- P4: write
    let started = Instant::now();
    write_users(&out.join("user_daily"), &acc.users, &ctx)?;
    write_campaigns(&out.join("campaign_daily"), &acc.camp, &ctx)?;
    phase("write", started);
    // Both writers have returned, so their output files are closed. `acc`, the
    // mmaps and the lookup maps are still live here; the callback is where the
    // binary exits instead of dropping them.
    on_success();
    Ok(true)
}

fn write_users(dir: &Path, users: &[types::UserRow], ctx: &Ctx) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    if users.is_empty() {
        return output::write_user_daily(dir, &[]);
    }
    let parts = rayon::current_num_threads().max(1);
    let per = users.len().div_ceil(parts).max(1);
    users
        .par_chunks(per)
        .enumerate()
        .try_for_each(|(i, rows)| {
            let mut names: Vec<[u8; 16]> = vec![[0; 16]; rows.len()];
            let rows: Vec<UserDaily> = rows
                .iter()
                .zip(names.iter_mut())
                .map(|(r, buf)| UserDaily {
                    user_id: id::unpack(r.person, buf),
                    day: r.day,
                    sessions: r.sessions,
                    events: r.events,
                    revenue_cents: r.revenue,
                    top_category: r.top_cat.map(|c| ctx.categories[c as usize]),
                })
                .collect();
            output::write_user_daily_named(dir, &format!("part-{i}.parquet"), &rows)
        })
}

fn write_campaigns(dir: &Path, camp: &[[i64; 4]], ctx: &Ctx) -> io::Result<()> {
    let organic = ctx.organic();
    let mut rows = Vec::new();
    for c in 0..=organic {
        for d in 0..ctx.n_days {
            let cell = camp[c * ctx.n_days + d];
            if cell[SESSIONS] == 0 && cell[ASSISTED] == 0 {
                continue;
            }
            let (campaign_id, channel) = if c == organic {
                (ORGANIC, ORGANIC)
            } else {
                (ctx.campaign_ids[c], ctx.channels[c])
            };
            rows.push(CampaignDaily {
                campaign_id,
                channel,
                day: ctx.day0 + d as i32,
                sessions: cell[SESSIONS],
                purchasing_sessions: cell[PURCHASING],
                revenue_cents: cell[REVENUE],
                assisted_sessions: cell[ASSISTED],
            });
        }
    }
    output::write_campaign_daily(dir, &rows)
}

#[cfg(test)]
mod tests {
    use super::dedup;
    use super::people;
    use super::types::{EvRaw, PURCHASE, VALID};
    use super::*;

    #[test]
    fn bits_default_to_eight_and_eight() {
        assert_eq!(resolve_bits(None, None, None), (8, 8));
        // Unset on one side only: that side still takes the default.
        assert_eq!(resolve_bits(None, Some("4"), None), (4, 8));
        assert_eq!(resolve_bits(None, None, Some("2")), (8, 2));
    }

    #[test]
    fn shorthand_sets_both_sides_when_alone() {
        assert_eq!(resolve_bits(Some("3"), None, None), (3, 3));
        assert_eq!(resolve_bits(Some("0"), None, None), (0, 0));
    }

    #[test]
    fn shorthand_plus_one_specialized_knob() {
        // A specialized knob overrides only its own side (S4-2).
        assert_eq!(resolve_bits(Some("5"), Some("1"), None), (1, 5));
        assert_eq!(resolve_bits(Some("5"), None, Some("2")), (5, 2));
    }

    #[test]
    fn both_specialized_knobs_ignore_the_shorthand() {
        assert_eq!(resolve_bits(Some("6"), Some("1"), Some("3")), (1, 3));
        assert_eq!(resolve_bits(Some("6"), Some("3"), Some("1")), (3, 1));
    }

    #[test]
    fn zero_bits_on_either_side_is_one_bucket() {
        assert_eq!(resolve_bits(None, Some("0"), None), (0, 8));
        assert_eq!(resolve_bits(None, None, Some("0")), (8, 0));
        assert_eq!(resolve_bits(Some("4"), Some("0"), Some("0")), (0, 0));
    }

    #[test]
    fn bits_are_bounded_like_the_legacy_knob() {
        assert_eq!(resolve_bits(Some("64"), None, None), (16, 16));
        assert_eq!(resolve_bits(None, Some("99"), Some("17")), (16, 16));
        // An unparseable knob behaves as unset, so the shorthand still applies.
        assert_eq!(resolve_bits(Some("3"), Some("x"), None), (3, 3));
    }

    fn pack(s: &str) -> u128 {
        id::pack(s.as_bytes()).unwrap()
    }

    fn person_map(pairs: &[(&str, &str)]) -> people::PersonMap {
        let pairs: Vec<(u128, u128)> = pairs.iter().map(|&(a, b)| (pack(a), pack(b))).collect();
        people::from_pairs(pairs.into_iter(), 0)
    }

    fn raw(id: u128, user: u128, ingest_s: i32, flags: u8) -> EvRaw {
        EvRaw {
            id,
            user,
            ingest_s,
            ts: 1_000 + ingest_s as u32,
            price: 10 + ingest_s as u32,
            cat: 2,
            flags,
        }
    }

    /// Scatter events by id at `dedup_bits`, dedup every event bucket and
    /// re-partition at `person_bits`, then gather the flat outputs the way P3
    /// reads them.
    fn route(
        rows: &[EvRaw],
        nparts: usize,
        dedup_bits: u32,
        person_bits: u32,
        pmap: &people::PersonMap,
    ) -> Vec<Vec<Ev>> {
        let nb_dedup = 1usize << dedup_bits;
        let nb_person = 1usize << person_bits;
        let mut parts: Vec<Vec<Vec<EvRaw>>> = (0..nparts)
            .map(|_| (0..nb_dedup).map(|_| Vec::new()).collect())
            .collect();
        for (i, r) in rows.iter().enumerate() {
            parts[i % nparts][id::bucket(r.id, dedup_bits)].push(*r);
        }
        let flats: Vec<dedup::Flat> = (0..nb_dedup)
            .map(|b| {
                dedup::partition_bucket(b, &parts, pmap, person_bits, nb_person)
                    .expect("offsets fit")
            })
            .collect();
        for f in &flats {
            assert_eq!(f.offsets().len(), nb_person + 1);
            assert_eq!(f.offsets()[0], 0);
            let lens: usize = (0..nb_person).map(|b| f.bucket(b).len()).sum();
            assert_eq!(*f.offsets().last().unwrap(), lens);
        }
        (0..nb_person)
            .map(|pb| {
                let evs: Vec<Ev> = flats
                    .iter()
                    .flat_map(|f| f.bucket(pb).iter().copied())
                    .collect();
                assert!(evs.iter().all(|e| id::bucket(e.person, person_bits) == pb));
                evs
            })
            .collect()
    }

    /// Independent oracle: smallest `ingest_s` per id wins, invalid winners are
    /// dropped (E5), survivors belong to their person (E4).
    fn expected(rows: &[EvRaw], person_bits: u32, pmap: &people::PersonMap) -> Vec<Vec<Ev>> {
        let nb_person = 1usize << person_bits;
        let mut best: std::collections::HashMap<u128, EvRaw> = std::collections::HashMap::new();
        for r in rows {
            match best.get_mut(&r.id) {
                Some(cur) if r.ingest_s < cur.ingest_s => *cur = *r,
                Some(_) => {}
                None => {
                    best.insert(r.id, *r);
                }
            }
        }
        let mut out: Vec<Vec<Ev>> = (0..nb_person).map(|_| Vec::new()).collect();
        for r in best.values() {
            if r.flags & VALID == 0 {
                continue;
            }
            let person = pmap.person(r.user);
            out[id::bucket(person, person_bits)].push(Ev {
                person,
                ts: r.ts,
                price: r.price,
                cat: r.cat,
                purchase: r.flags & PURCHASE != 0,
            });
        }
        out
    }

    fn sorted(mut v: Vec<Vec<Ev>>) -> Vec<Vec<Ev>> {
        for bucket in &mut v {
            bucket.sort_unstable_by_key(|e| (e.person, e.ts, e.price, e.cat, e.purchase));
        }
        v
    }

    #[test]
    fn asymmetric_bits_route_every_event_to_its_person_bucket() {
        // u1/u3/u9 are one person named u1; u5/u7 are one person named u5.
        let pmap = person_map(&[("u1", "u9"), ("u3", "u9"), ("u5", "u7")]);
        let users = ["u0", "u1", "u3", "u5", "u7", "u9"];
        let mut rows = Vec::new();
        for k in 0..40usize {
            let idv = pack(&format!("e{k}"));
            for c in 0..(1 + k % 3) {
                let user = users[(k * 7 + c * 5) % users.len()];
                // Some ids get an invalid earlier copy: dedup keeps it, E5 drops it.
                let flags = if (k + c) % 11 == 0 {
                    0
                } else {
                    VALID | if c % 2 == 0 { PURCHASE } else { 0 }
                };
                rows.push(raw(idv, pack(user), ((k * 3 + c) % 5) as i32, flags));
            }
        }
        rows.reverse();
        // The input only proves routing if event bucket and person bucket differ.
        assert!(rows
            .iter()
            .any(|r| id::bucket(r.id, 1) != id::bucket(pmap.person(r.user), 3)));

        for &(db, pb) in &[(1u32, 3u32), (3, 1), (0, 3), (3, 0)] {
            let got = sorted(route(&rows, 3, db, pb, &pmap));
            assert_eq!(got.len(), 1usize << pb);
            assert_eq!(got, sorted(expected(&rows, pb, &pmap)), "dedup={db} person={pb}");
        }
        // (1,3) and (3,1) hold the same events; only the person split differs.
        let events = |db, pb| {
            let mut v: Vec<Ev> = route(&rows, 3, db, pb, &pmap).into_iter().flatten().collect();
            v.sort_unstable_by_key(|e| (e.person, e.ts, e.price, e.cat, e.purchase));
            v
        };
        assert_eq!(events(1, 3), events(3, 1));
    }

    #[test]
    fn prealloc_cap_matches_formula() {
        // 70 MB of events, 16 workers, 256 buckets: 256 rows/worker/bucket, +15%.
        assert_eq!(prealloc_cap(70 << 20, 70, 16, 256), 294);
        assert_eq!(prealloc_cap(0, 70, 16, 256), 0);
        // Degenerate divisors never panic or divide by zero.
        assert_eq!(prealloc_cap(700, 0, 0, 0), 700 * 115 / 100);
    }

    #[test]
    fn bucket_lens_sums_every_worker() {
        // Two workers, four buckets: counts add up across parts, empties stay 0.
        let parts: Vec<Vec<Vec<u8>>> = vec![
            vec![vec![1, 2], vec![], vec![3], vec![4]],
            vec![vec![5], vec![], vec![], vec![]],
        ];
        assert_eq!(bucket_lens(&parts, 4), vec![3, 0, 1, 1]);
        assert_eq!(bucket_lens::<u8>(&[], 2), vec![0, 0]);
    }

    #[test]
    fn bucket_order_is_largest_first_then_index() {
        assert_eq!(bucket_order(&[3, 9, 3, 0, 9]), vec![1, 4, 0, 2, 3]);
        assert_eq!(bucket_order(&[0, 0, 0]), vec![0, 1, 2]);
        assert!(bucket_order(&[]).is_empty());
    }

    #[test]
    fn scatter_visits_every_chunk_once() {
        // Four chunks of uneven size, more workers than chunks: every line must
        // land in exactly one bucket, and every worker owns a full bucket set.
        let chunks: Vec<&[u8]> = vec![b"a\nb\n", b"", b"c\n", b"d\ne\nf\n"];
        let nb = 4;
        let overflow = AtomicBool::new(false);
        let parts = scatter(&chunks, nb, 1, &overflow, |line| {
            Parsed::Rec(((line[0] as usize - 'a' as usize) % nb, line[0]))
        });
        assert!(!overflow.load(Ordering::Relaxed));
        assert_eq!(parts.len(), rayon::current_num_threads().max(1));
        assert!(parts.iter().all(|p| p.len() == nb));
        let mut got: Vec<u8> = parts.iter().flatten().flatten().copied().collect();
        got.sort_unstable();
        assert_eq!(got, b"abcdef");
    }

    #[test]
    fn scatter_reports_overflow() {
        let chunks: Vec<&[u8]> = vec![b"a\n", b"b\n"];
        let overflow = AtomicBool::new(false);
        let parts = scatter::<u8, _>(&chunks, 2, 0, &overflow, |line| {
            if line[0] == b'a' {
                Parsed::Overflow
            } else {
                Parsed::Rec((1, b'b'))
            }
        });
        assert!(overflow.load(Ordering::Relaxed));
        assert_eq!(parts.iter().flatten().flatten().count(), 1);
    }
}
