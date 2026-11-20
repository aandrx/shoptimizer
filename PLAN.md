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

- **Image:** multi-stage build → static binary in `FROM scratch`/distroless. Build with `-C target-cpu=x86-64-v4`, LTO, `panic=abort`, mimalloc.
- **Read/parse:** mmap files, split chunks at newlines across 8 threads (not one file per thread: the largest file holds ~1/5 of the rows), `memchr`/SIMD scanning, hand-written fixed-offset timestamp parsers with days-from-civil validation. Parse only needed fields.
- **IDs as packed `u128`, not an interning table:** every event/user/product ID in the data is 8–11 bytes, so pack the bytes into a `u128` (length in the top byte). Exact equality and byte-wise order with no shared, contended dictionary; IDs over 15 bytes go to a rare slow path.
- **Dedup:** partition events by a hash of the packed `event_id` so each thread dedups its own shard lock-free. Carry a valid flag through dedup (validation happens after).
- **Then re-partition by person.** Dedup buckets are keyed by event, sessions by person: a second scatter pass is required. About 1 in 64 duplicated events name a different user in later copies, and the earliest copy's user is the one that counts.
- **Skip click dedup:** copies of a click have identical values, so they cannot change the credit (a tie resolves to the same `click_id`) or assists (each campaign counts once).
- **Links:** union-find over interned IDs; min ID per root.
- **Sessionize:** sort compact records (person, ts, type, category, price) per partition; one linear pass.
- **Attribution/assists:** sort clicks by `(person, ts, click_id)` so the credit tie-break falls out of the order; sliding 7-day window over each person's clicks with per-campaign counts — O(n) even for whales and shared devices.
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

## Risks

1. **Correctness** — the biggest risk. Differential testing against the reference is mandatory.
2. **Disk I/O floor** — if `/data` isn't page-cached, reading ~6 GB may cost seconds no matter what. Check scaling on early practice submissions.
3. **Competition** — others will go native too; the race is decided by parse speed, lock-free partitioning, allocations, and I/O.
4. **Time** — deadline is ~1.5 days out. The reference is done; the next milestone is a simple single-threaded Rust version that matches it, submitted **today** (ties go to the earlier submission, 2 submissions/hour). The Python reference is not submittable: ~13 s per 1M events would be ~4 min and far too much memory at 20M.

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
