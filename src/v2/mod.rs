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
use std::sync::atomic::{AtomicBool, Ordering};
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

fn phase(name: &str, started: Instant) {
    eprintln!("[phase] {name} {}ms", started.elapsed().as_millis());
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
        || -> Vec<Vec<Vec<EvRaw>>> {
            ev_chunks
                .par_iter()
                .fold(
                    || vec![Vec::new(); nb],
                    |mut acc: Vec<Vec<EvRaw>>, c| {
                        for line in scan::lines(c) {
                            match parse::event(line, &ctx) {
                                Parsed::Rec(r) => acc[id::bucket(r.id, bits)].push(r),
                                Parsed::Skip => {}
                                Parsed::Overflow => overflow.store(true, Ordering::Relaxed),
                            }
                        }
                        acc
                    },
                )
                .collect()
        },
    );
    let Some(people) = people else {
        return Ok(false);
    };
    phase("links+events", started);
    let started = Instant::now();
    let ck_chunks = scan::chunks(&click_maps, chunk);
    let ck_parts: Vec<Vec<Vec<Ck>>> = ck_chunks
        .par_iter()
        .fold(
            || vec![Vec::new(); nb],
            |mut acc: Vec<Vec<Ck>>, c| {
                for line in scan::lines(c) {
                    match parse::click(line, &ctx) {
                        Parsed::Rec((user, mut ck)) => {
                            ck.person = people.person(user);
                            acc[id::bucket(ck.person, bits)].push(ck);
                        }
                        Parsed::Skip => {}
                        Parsed::Overflow => overflow.store(true, Ordering::Relaxed),
                    }
                }
                acc
            },
        )
        .collect();
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
            let mut out: Vec<Vec<Ev>> = vec![Vec::new(); nb];
            for r in kept.values() {
                if r.flags & VALID == 0 {
                    continue;
                }
                let person = people.person(r.user);
                out[id::bucket(person, bits)].push(Ev {
                    person,
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
