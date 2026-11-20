# Shoptimizer — Plan for the Shopping Sessions track

Track: https://www.joinbuilders.org/buildathon/tracks/shopping-sessions (Databricks, $5,000, one winner)
Deadline: **Sun Nov 22, 11:00 AM ET**. Full rules: [`rules.md`](rules.md) — it wins over anything here.

## The task

Turn messy click-stream CSVs into two exact daily Parquet tables (`user_daily`, `campaign_daily`).
**Score = wall-clock time, container start to exit. Output must match the reference exactly — one wrong row scores nothing.**

Pipeline, in rule order:

1. Parse CSV with a custom dialect (quotes are syntax, `""` is null, `\r` is data, malformed lines dropped, no trimming or case folding).
2. Parse timestamps in three formats (F1 UTC, F2 offset, F3 epoch ms) with strict validation.
3. Merge user IDs into people via `links/` (transitive union-find; name = byte-wise smallest ID).
4. Events: dedup on `event_id` (earliest `ingest_ts`) **before** validation → validate → drop >24 h late.
5. Sessionize per person (gap > 30 min; equal ts share a session); day = UTC date of session start.
6. Credit each session to the latest valid click within 7 days at or before its start (tie → smallest `click_id`), else `organic`.
7. Assists: every distinct campaign clicked in that 7-day window.
8. Write both tables (`day` as Parquet DATE, no extra columns).

## Environment and scoring

- linux/amd64, GCP n2 Xeon (AVX-512), 8 vCPU, 24 GB RAM, no swap, no network at run time.
- `/data` read-only; write `/output/{user_daily,campaign_daily}/`; `/scratch` for temp; root FS read-only (`/tmp` 64 MB); uid 1000.
- Repo public, ≤ 50 MB at the scored commit, `Dockerfile` at the top.
- Dev gate: public 1M-event set, must finish < 3 min and match.
- Practice board: hidden ~20M events (median of 3 if < 20 s).
- **Final: fresh ~60M events, 3-min cap per run, median of 3**; top 5+ re-timed 5× on a quiet machine.
- Hidden-data feedback: exit code, OOM flag, counts of missing/extra/differing rows only.
- 2 submissions/hour. ~20% machine variance. Ties go to the earlier submission.

| | Time |
|---|---|
| Baseline (PySpark) | 171.91 s |
| Leader at time of writing | 16.38 s |

## Approach

### 1. Correctness first

- Write a **simple reference implementation** that reads like `rules.md` (clarity over speed).
- It must pass all **36 fixtures** and match **`dev-answer`** exactly.
- Use it as the oracle: generate many seeds with the generator (windows, Feb 29, dirt rates and file layouts vary) and diff the fast pipeline against it on every one.
- Submit the correct version early to bank a score and verify the end-to-end flow.

### 2. Then speed: one static Rust binary

Why not DuckDB/Polars/Spark: the work is dominated by parsing ~5–6 GB of CSV whose dialect those readers don't follow, and Python/JVM startup is timed.

- **Image:** multi-stage build → static binary in `FROM scratch`/distroless. Build with `-C target-cpu=x86-64-v3` (A/B `v4`, see risk 6), LTO, `panic=abort`, mimalloc. Minimal dependencies (risk 5).
- **Read/parse:** mmap files, split into fixed ~16–64 MB byte ranges aligned to newlines (not one file per thread: the largest file holds ~1/6–1/5 of the rows) across 8 threads, `memchr`/SIMD scanning, hand-written fixed-offset timestamp parsers with days-from-civil validation. Parse only needed fields.
- **IDs as packed `u128`, not an interning table:** every event/user/click/link ID in generated data is ≤ 9 bytes (measured on 1M and 20M sets; product IDs ≤ 6), so pack the bytes into a `u128` with no shared, contended dictionary. **Layout matters for ordering:** put the bytes big-endian from the top, zero-padded, with the length in the *lowest* byte. Then integer `<` equals byte-wise order (a prefix sorts first). Length in the top byte would sort by length first and break person names and the `click_id` tie-break. IDs over 15 bytes go to a rare slow path that must keep the same ordering.
- **Dedup:** partition events by a hash of the packed `event_id` so each thread dedups its own shard lock-free. Carry a valid flag through dedup (validation happens after).
- **Then re-partition by person.** Dedup buckets are keyed by event, sessions by person: a second scatter pass is required. About 1 in 64 duplicated events name a different user in later copies, and the earliest copy's user is the one that counts.
- **Skip click dedup:** copies of a click have identical values, so they cannot change the credit (a tie resolves to the same `click_id`) or assists (each campaign counts once, so the per-campaign window counts only need to be > 0).
- **Links:** union-find over packed IDs (hash map packed ID → index); min ID per root.
- **Sessionize:** sort compact records (person, ts, type, category, price) per partition; one linear pass.
- **Attribution/assists:** sort clicks by `(person, ts, click_id)`. The credit is the **first** click of the last `ts` group at or before the session start, not simply the last click ≤ start (that would pick the *largest* `click_id` on a tie). Sliding 7-day window over each person's clicks with per-campaign counts — O(n) even for whales and shared devices.
- **Output:** `campaign_daily` is tiny; write `user_daily` as several uncompressed Parquet files in parallel.
- **Memory:** keep peak ≲ 10 GB at 60M events.

### Ideas for an edge

- Overlap I/O with parsing (stream chunks, don't load everything first).
- Drop out-of-window / bad-type rows early (but they still take part in dedup).
- Minimal hand-rolled Parquet writer if the crate's overhead shows up.
- Optimize for the 60M final, not just the 20M practice set.

### Traps that need a named test

- F3 with any number of leading zeros (`0001791071890000`), or longer than an i64: not malformed, just out of window.
- Offsets: `-00:00` and `+23:59` valid; `+24:00`, `+05:60`, `+0200` bad. Feb 29 valid only in leap years (some windows hold 2028-02-29).
- Dedup before validation: an invalid earliest copy drops the event; a later valid copy does not rescue it. A malformed copy never counts.
- Equal timestamps share a session regardless of read order (E16).
- Links: a data row equal to the header is data (L3); upper-cased IDs sort before lowercase and can become the name; self-links and repeats are no-ops.
- Header-only files; a final line without `\n`; `\r` is data (`1000\r` is a bad price).
- Packed-ID ordering: `U1` < `u1` < `u1 ` < `u10` < `u2` must hold as integers.
- Credit tie at the same `ts`: smallest `click_id` wins (A5), not the last in sort order.

## Measured data sizes

Generated locally with the published generator:

| | 1M (dev, seed 1) | 20M (seed 7) | 60M final (estimate) |
|---|---|---|---|
| Total on disk | 92 MB | 1.8 GB | **~5.5 GB** |
| Event row size | ~77 bytes | | |
| Event files | 30 | 143 | |
| Largest single file | | 287 MB (~16% of all data) | |
| Parallel read of all files (page-cached) | | 0.1–0.2 s | |

## Risks

### 1. Correctness — the biggest risk

Rules most likely to silently break a fast pipeline:

- **Dedup is global and comes first.** The kept copy can name a *different user* than later copies (fixture E4; ~1 in 64 duplicated events). Never partition by person before deduping by `event_id`.
- **An invalid kept copy kills the event** even if a later copy is valid (E5; 2% of duplicated events).
- **Only the first line is the header.** A later line identical to the header is data (L3).
- **F3 overflow.** A 21-digit F3 is out of window, not malformed — the parser must detect overflow, not crash or wrap.
- **Upper-cased IDs sort first** and can rename a person. Byte-wise compare everywhere, never fold case.
- **Equal timestamps around a 30-min gap** share a session (E16): measure gaps between distinct timestamps.

See also "Traps that need a named test" above.

Mitigation: the reference already passes all 36 fixtures + `dev-answer`; run `make diff-seeds` on 20+ seeds against every fast-pipeline change (window incl. Feb 29 2028, dirt rates, and file layout vary per seed). A 20M set generates in ~3 s, so a large seed sweep is cheap.

### 2. Disk I/O floor — uncertain until measured on the scorer

Page-cached, reading 5.5 GB is well under a second, so CPU parsing is the floor. On a cold GCP persistent disk (~240 MB/s–1.2 GB/s) reading alone could cost 5–20 s, outside our control.

**File sizes are heavily skewed** (one file ≈ 16% of the data). Per-file work assignment leaves 7 threads idle. **Split work into fixed byte ranges (~16–64 MB) aligned to newlines**, not one task per file.

Mitigation: log per-phase timings to stderr; compare practice-board time against a local 20M run to tell I/O-bound from CPU-bound.

### 3. Competition — winnable

Rough budget for 60M events on 8 cores, page-cached:

| Phase | Estimate |
|---|---|
| Parse 5.5 GB (0.5–1 GB/s/core) | 0.7–1.4 s |
| Dedup ~64M IDs (hash) | ~0.5 s |
| Sort, sessionize, attribute | ~0.5–1 s |
| Parquet write + container start | ~0.3 s |
| **Total** | **~2–4 s** |

Skew is fine: a whale is ~1.2M events, the shared-device person ~250k; each sorts in tens of ms.

Among native entries the race is decided by parser speed, zero per-row allocations, a pre-sized lock-free dedup table, and even work splitting.

### 4. Time — on track, submit today

Deadline is ~1.5 days out. The reference is done; the next milestone is a simple single-threaded Rust version that matches it, submitted **today** (ties go to the earlier submission). The Python reference is not submittable: ~13 s per 1M events would be ~4 min and far too much memory at 20M. 2 submissions/hour ≈ 80 total; plenty if we test locally first.

### 5. Image build must finish in 10 minutes on the scorer

A Rust build pulling `arrow`/`parquet` can take minutes on 8 cores. Keep dependencies minimal: `parquet` with default features off, or a small hand-written Parquet writer (two tables, few fixed-type columns). Cache dependency builds in their own Docker layer.

### 6. AVX-512 may hurt on Cascade Lake

Heavy AVX-512 use downclocks Cascade Lake Xeons; our local Zen 4 has no such penalty, so local results will flatter it. Build for `x86-64-v3` (AVX2) by default and A/B `x86-64-v4` on the practice board before switching.

## Setup checklist

`make setup` does the venv, generator and dev-answer steps below. See the Makefile for `fixtures`, `check-dev`, `diff-seeds` (each takes `CAND=...`).

- [ ] Install Docker buildx (`make build` needs it; Arch: `sudo pacman -S docker-buildx`).
- [x] Build the generator: unzip `generator.zip`, `cargo build --release`, put `generate` on PATH.
- [ ] Generate dev set: `generate --seed 1 --events 1000000 --clicks 250000 --output dev`; plus a 20M set for timing (the 60M set is ~5.5 GB on disk).
- [x] `uv pip install polars` for `scripts/compare.py` and `scripts/run_fixtures.py`.
- [x] Copy the starter harness (`fixtures/`, `scripts/`, `Makefile`, `.dockerignore`) from a starter zip.
- [ ] Rust crate + two-stage Dockerfile.
- [x] `.gitignore` for `dev/`, `out/`, `target/`, generated data; unzip `dev-answer` locally for `make check-dev`.
- [ ] Keep the repo public; fill in `TEAM.md`.

## Milestones

1. ~~Harness + reference passing fixtures and `dev-answer`~~ — done: `reference/reference.py` passes 36/36 fixtures and matches `dev-answer` exactly (13 s on 1M events).
2. ~~Differential test harness across many generator seeds~~ — done: `scripts/diff_seeds.py` / `make diff-seeds CAND=...`.
3. Fast Rust pipeline matching the reference → **submit**.
4. Profile on 20M/60M, tune, resubmit (≤ 2/hour).
