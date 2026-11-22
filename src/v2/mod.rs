//! V2 parallel pipeline (PLAN.md §6, Submission #2).
//!
//! P0 lookups -> P1 parse + scatter -> P2 dedup + re-partition by person ->
//! P3 sessions -> P4 write. Falls back to [`crate::v1::run`] whenever the input
//! doesn't fit V2's compact encoding (D6, D13).

pub mod parse;
pub mod people;
pub mod scan;
pub mod sessions;
pub mod types;

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;
use rayon::prelude::*;

use crate::agg::ORGANIC;
use crate::id;
use crate::output::{self, CampaignDaily, UserDaily};
use crate::ts::Window;

use sessions::{Acc, ASSISTED, PURCHASING, REVENUE, SESSIONS};
use types::{Ck, Ctx, Ev, EvRaw, Parsed, PURCHASE, VALID};

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

/// Initial capacity for one worker's bucket `Vec` (S3-5):
/// `total_bytes / bytes_per_row / workers / nb * 1.15`.
///
/// Only a hint: under-estimates grow normally, so correctness never depends on
/// it. `bytes_per_row` is the observed average row size (events 70, clicks 45).
fn prealloc_cap(total_bytes: usize, bytes_per_row: usize, workers: usize, nb: usize) -> usize {
    let rows = total_bytes / bytes_per_row.max(1);
    (rows as u64 * 115 / (100 * workers.max(1) as u64 * nb.max(1) as u64)) as usize
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
    let bits = env_usize("SHOPT_BUCKET_BITS", 8).min(16) as u32;
    let nb = 1usize << bits;
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
            let cap = prealloc_cap(bytes, 70, workers, nb);
            scatter(&ev_chunks, nb, cap, &overflow, |line| {
                match parse::event(line, &ctx) {
                    Parsed::Rec(r) => Parsed::Rec((id::bucket(r.id, bits), r)),
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
        let cap = prealloc_cap(bytes, 45, workers, nb);
        scatter(&ck_chunks, nb, cap, &overflow, |line| {
            match parse::click(line, &ctx) {
                Parsed::Rec((user, mut ck)) => {
                    ck.person = people.person(user);
                    Parsed::Rec((id::bucket(ck.person, bits), ck))
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

    // ---- P2: dedup per event bucket, then re-partition by person
    let started = Instant::now();
    let ev_out: Vec<Vec<Vec<Ev>>> = (0..nb)
        .into_par_iter()
        .map(|b| {
            let total: usize = ev_parts.iter().map(|p| p[b].len()).sum();
            let mut kept: HashMap<u128, EvRaw> = HashMap::with_capacity(total);
            for part in &ev_parts {
                for r in &part[b] {
                    match kept.entry(r.id) {
                        Entry::Occupied(mut o) => {
                            // Equal ingest_ts copies are identical: either is fine.
                            if r.ingest_s < o.get().ingest_s {
                                o.insert(*r);
                            }
                        }
                        Entry::Vacant(v) => {
                            v.insert(*r);
                        }
                    }
                }
            }
            // Count VALID survivors per destination person bucket, so each output
            // Vec gets its exact capacity and never doubles (S3-5).
            //
            // The dedup map's `user` field is dead after this point, so each
            // survivor's person is written back into it; the fill pass then needs
            // no second `people.person` lookup.
            let mut counts = vec![0usize; nb];
            for r in kept.values_mut() {
                if r.flags & VALID == 0 {
                    continue;
                }
                r.user = people.person(r.user);
                counts[id::bucket(r.user, bits)] += 1;
            }
            let mut out: Vec<Vec<Ev>> = counts.into_iter().map(Vec::with_capacity).collect();
            for r in kept.values() {
                if r.flags & VALID == 0 {
                    continue;
                }
                out[id::bucket(r.user, bits)].push(Ev {
                    person: r.user,
                    ts: r.ts,
                    price: r.price,
                    cat: r.cat,
                    purchase: r.flags & PURCHASE != 0,
                });
            }
            out
        })
        .collect();
    // S3-3: free the parse buffers in the background instead of on the critical path.
    std::thread::spawn(move || drop(ev_parts));
    phase("dedup", started);

    // ---- P3: sessions per person bucket
    let started = Instant::now();
    let acc = (0..nb)
        .into_par_iter()
        .fold(
            || Acc::new(&ctx),
            |mut acc, b| {
                let n: usize = ev_out.iter().map(|o| o[b].len()).sum();
                let mut evs: Vec<Ev> = Vec::with_capacity(n);
                for o in &ev_out {
                    evs.extend_from_slice(&o[b]);
                }
                let n: usize = ck_parts.iter().map(|p| p[b].len()).sum();
                let mut cks: Vec<Ck> = Vec::with_capacity(n);
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
    use super::*;

    #[test]
    fn prealloc_cap_matches_formula() {
        // 70 MB of events, 16 workers, 256 buckets: 256 rows/worker/bucket, +15%.
        assert_eq!(prealloc_cap(70 << 20, 70, 16, 256), 294);
        assert_eq!(prealloc_cap(0, 70, 16, 256), 0);
        // Degenerate divisors never panic or divide by zero.
        assert_eq!(prealloc_cap(700, 0, 0, 0), 700 * 115 / 100);
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
