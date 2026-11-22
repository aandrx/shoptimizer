# Shoptimizer — Execution Plan

Track: https://www.joinbuilders.org/buildathon/tracks/shopping-sessions (Databricks, $5,000, one winner).
**Deadline: Sun Nov 22 2026, 11:00 AM ET. Code freeze: Sun 09:00 AM ET.**

---

## 0. Read me first (for the orchestrating session)

This file is written so one orchestrator session can hand each task in §6 to a subagent with no other context.

**Read order:** this file → [`rules.md`](rules.md) (the spec, authoritative) → [`fixtures/README.md`](fixtures/README.md) (36 edge cases) → [`reference/reference.py`](reference/reference.py) (correct, slow oracle).

**Roles:**
- **Orchestrator:** picks ready tasks from §6, spawns one subagent per task, passes it the task card plus §0–§5, checks the acceptance output, merges.
- **Subagent:** does exactly one task card. Touches only the files in its *Scope*. Reports the acceptance commands' output verbatim.
- **Human:** does the `H*` tasks (sudo, GitHub, submissions). Agents cannot do these.

**Rules of engagement:**
1. `rules.md` wins over everything, this file included. Never edit `rules.md`, `fixtures/`, or the behaviour of `reference/reference.py`. If the reference looks wrong against `rules.md`, stop and escalate to the human with a minimal repro.
2. One task = one branch `t<NN>-<slug>` (use a git worktree for parallel tasks). Commit locally. Only the orchestrator merges to `main`, and only after the task's acceptance passes on the merged result.
3. A task is **done** only when every acceptance command passes. "Should work" is not done.
4. Every change to pipeline code must pass gate **G1** (§7). Performance tasks must also pass **G3**.
5. **Sandbox:** GitHub access (push, pull, fetch) depends on the machine: some agent sandboxes have it, some don't. Check with `git fetch` before assuming either way. Agents never use `sudo`. Hand those to the human.
6. Do not relitigate §4 decisions. If a task seems to need a different decision, stop and ask the orchestrator.
7. The pipeline writes logs to **stderr only**. Stdout stays effectively empty (the scorer caps it at 50 MB).
8. Never commit `data/`, `.tools/`, `.venv/`, `target/`, `out*/`, or generated datasets. The repo must stay < 50 MB.
9. Dependencies are limited to the allow-list in §4. Anything else needs orchestrator approval (risk: the 10-minute scorer build).

---

## 1. Goal and scoring

Read messy CSVs from `/data`, write two Parquet tables to `/output/user_daily/` and `/output/campaign_daily/`, exactly as `rules.md` defines.

- **Score = wall-clock seconds, container start → exit. Lower wins.** Any wrong row scores nothing.
- Run box: linux/amd64 GCP n2 Xeon (Cascade Lake or newer), **8 vCPU (= 4 physical cores with hyperthreads, so per-core parse speed matters), 24 GB RAM, no swap, no network**. `/data` read-only, `/scratch` writable, root FS read-only (`/tmp` 64 MB), uid 1000. Env: `DATA_DIR OUTPUT_DIR SCRATCH_DIR NCPU=8 MEM_GB=24`.
- Gate: public dev set (1M events) must match, < 3 min. Practice board: hidden ~20M events. **Final: fresh ~60M events, 3-min cap per run, median of 3**; the top 5+ are re-timed 5×.
- Hidden-data feedback is only exit code, OOM flag, and counts of missing/extra/differing rows.
- Limits: **3 submissions/hour** (was 2; can change), a rejected entry still counts, ties go to the earlier submission, ~20% machine variance. See §8 for the form.
- Board (Sat Nov 21): 317 entries; the top ~50 are 0.28–0.69 s, with ~15 teams at 0.29–0.31 s. That's at the container-start floor, so these are almost certainly dataset-specific shortcuts, not full pipelines. **Don't chase the practice board.** The final runs on fresh 60M data. See §9.
- Image build on the scorer has network, **≤ 10 minutes**.

---

## 2. Current state

| Path | What | Status |
|---|---|---|
| `rules.md` | The spec | given, frozen |
| `fixtures/` | 36 edge cases: `input/` + `expected/*.csv` | given, frozen |
| `reference/reference.py` | Literal Python implementation. 36/36 fixtures, matches `dev-answer`. ~13 s and **~740 MB peak RSS per 1M events** (measured) | done, frozen |
| `scripts/run_fixtures.py` | Runs `CMD` on every fixture, exact diff | done |
| `scripts/compare.py` | Exact order-insensitive diff of two output folders | done |
| `scripts/diff_seeds.py` | Runs candidate + reference on many generator seeds, diffs (cached in `data/`) | done |
| `scripts/docker_run.sh` | Runs the image with the scorer's exact flags | done |
| `scripts/build_generator.sh` | Builds `generator.zip` → `.tools/generate` | done |
| `Makefile` | `setup dev fixtures check-dev diff-seeds build docker-fixtures run-dev` | done |
| `dev-answer.zip` | Expected output for seed 1, 1M events (117,038 user rows, 27,400 campaign rows) | given |
| `Cargo.toml`, `src/`, `Dockerfile` | Rust pipeline. **V1** (`src/v1.rs`, single-threaded) scored 40.95 s on hidden 20M with an exact match. V2 (`src/v2/`) is the next submission (§6). | V1 done |

**Candidate CLI convention** (used by every script): the command gets `DATA_DIR` and `OUTPUT_DIR` both as env vars **and** as its last two arguments. The Rust binary must accept `shoptimizer [DATA_DIR OUTPUT_DIR]`, falling back to the env vars (the scorer passes no arguments).

---

## 3. Command cheat sheet

```sh
make setup                                    # .venv with polars, .tools/generate, unzip dev-answer/
make dev                                      # data/dev: seed 1, 1M events (the public dev set)
.tools/generate --seed 7 --events 20000000 --clicks 5000000 --output data/p20   # practice-size set, ~1.8 GB
.tools/generate --seed 9 --events 60000000 --clicks 15000000 --output data/p60  # final-size set, ~5.5 GB

C=target/release/shoptimizer                  # the candidate once it exists
make fixtures   CAND=$C                       # 36 fixtures, native
make check-dev  CAND=$C                       # dev set vs dev-answer, native, timed
make diff-seeds CAND=$C                       # seeds 2-30 × 200k events vs reference
make diff-seeds CAND=$C SEEDS=31-60           # more seeds
.venv/bin/python scripts/diff_seeds.py --seeds 41-43 --events 2000000 --clicks 500000 -- $C   # bigger sets

make build                                    # docker image, linux/amd64 (needs docker-buildx)
make docker-fixtures                          # 36 fixtures through the image with scorer flags
make run-dev                                  # dev set through the image, timed, diffed
CPUS=8 MEM=24g scripts/docker_run.sh data/p20 out-p20   # timed practice-size run
```

---

## 4. Fixed decisions (do not relitigate)

| # | Decision | Why |
|---|---|---|
| D1 | **Rust, one binary.** No DuckDB/Polars/Spark/Python at run time. | The work is CSV parsing whose dialect those readers don't follow; Python/JVM startup is timed. |
| D2 | **V1 first:** simple, single-threaded, `&[u8]`/`Vec<u8>` IDs, correct. Submit it. **V2** adds speed behind the same tests. | Bank a correct score early (ties go to the earlier submission). V1 stays as the fallback path. |
| D3 | Dependency allow-list: `parquet` (`default-features = false`, no arrow, uncompressed), `rayon`, `memmap2`, `mimalloc`, `hashbrown`, `memchr` (added for S3-4). Nothing else without approval. | Scorer build ≤ 10 min. |
| D4 | Docker: builder `rust:1.97-bookworm` (matches local rustc 1.97) with a dependency-cache layer; runtime `gcr.io/distroless/cc-debian12`; `ENTRYPOINT ["/shoptimizer"]`. | Small image, fast start. Distroless has no `chmod`, so `scripts/docker_run.sh` cleans up with `busybox:1.36` instead (fixed). |
| D5 | Build flags in `.cargo/config.toml`: `target-cpu=x86-64-v3`, plus release profile `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`. `x86-64-v4` only via the T13 A/B. | AVX-512 downclocks Cascade Lake. Local Zen 4 has no penalty, so local tests flatter v4. |
| D6 | **Packed IDs (V2):** `u128`, ID bytes big-endian from bit 127 down, zero-padded, **length in the lowest byte**. Integer `<` then equals byte-wise order. IDs > 15 bytes: the whole run falls back to the V1 path (a flag set during parse). | No shared interning table. Measured: all IDs ≤ 9 bytes in generated data, so the fallback is for safety only. |
| D7 | **No click dedup.** Copies are identical, so they can't change credit or assists. Assist window counts only need `> 0`. | Saves a hash pass over ~15M clicks. |
| D8 | **Dedup events globally first** (partition by `event_id` hash), **then** re-partition by person. | The kept copy may name a different user (E4). |
| D9 | Parallel parse splits files into **fixed byte ranges aligned to `\n`**, size from env `SHOPT_CHUNK_BYTES` (default 32 MiB). | File sizes are skewed (one file ≈ 16% of the data). The env var lets small tests exercise many chunks. |
| D10 | Parquet: `INT64` for counts, `INT32` + logical `DATE` for `day`, `BYTE_ARRAY` + `String` for strings, `top_category` nullable, no compression. | Matches the checker. Cheap to write. |
| D11 | **Oracle by dataset size.** `reference/reference.py` up to ~2M events. Above that, the **V1 binary** (gate G4). V1 is also the **runtime fallback** for V2. Never change V1's behaviour. | V1 matched the hidden 20M set exactly. The Python reference needs ~740 MB per 1M events (~15 GB at 20M, ~45 GB at 60M), so it can't run at those sizes on a laptop. |
| D12 | **Copy validity is computed at parse.** Every check in rules.md Events steps 3–4 (incl. late) depends only on the copy itself, so dedup only compares `(event_id, ingest)` and keeps the record with its `VALID` flag. | Dedup becomes a tiny per-bucket operation. |
| D13 | Fixed-size records (`EvRaw`, `Ev`, `Ck`, §6; §5a O7) with `u128` IDs stored as `[u64; 2]`. Timestamps are `u32` ms offsets (event: from `window_start`; click: from `window_start − 7 d`). `ingest_ts` (always whole-second F1) is stored as `i32` **seconds** from `window_start`, exact even on invalid copies, because dedup compares it across copies. The late check runs at parse on full-precision values. Fall back to V1 if any `ingest_ts` is out of `i32` range, or the window + 7 d doesn't fit in `u32` ms (~49 days). | Smaller records mean less memory traffic on every scatter and sort. 21 days = 1.8e9 ms fits. |
| D14 | Pass structure P0–P4 (§6) with `2^B` buckets, `B` from `SHOPT_BUCKET_BITS` (default 8). Per-bucket `hashbrown` tables pre-sized to bucket length. | Each bucket's table fits in L2; no locks. |
| D15 | Assists: per-worker count array over campaign index + active list with swap-remove. Never rescan the click window per session. | The shared-device person has thousands of clicks in a window. |
| D16 | `user_daily` rows fold in-stream (a person's sessions arrive in ts order, so days don't decrease). `campaign_daily` uses dense per-worker `[campaign][day]` arrays merged at the end. No output hash maps. | Removes V1's slowest random-access maps. |
| D17 | Category indices are assigned in **byte-wise order** of category name (with `unknown` inserted in sorted position, sharing the index if `products.csv` already uses it), so the `top_category` tie-break is "smallest index". | Integer compare instead of string compare. |

---

## 5. Architecture and module contracts

Crate `shoptimizer`, binary `shoptimizer`. Modules and their public contracts. Subagents implement against these signatures, so the modules can be built in parallel.

```rust
// src/main.rs — arg/env handling, read manifest, run phases, log "[phase] <name> <ms>" to stderr.
//               Exit code 0 on success, non-zero on any I/O error.

// src/csv.rs — rules.md "CSV dialect".
/// Data lines of one file's bytes: skips the first line (header); only b'\n' ends a line;
/// text after the last '\n' is a line if non-empty; a trailing '\r' stays in the line.
pub fn data_lines(buf: &[u8]) -> impl Iterator<Item = &[u8]>;
/// Exactly N comma-separated fields, or None if malformed (wrong count, empty line,
/// a quote anywhere except wrapping a whole field). Field is None for null ("" or empty).
pub fn split<const N: usize>(line: &[u8]) -> Option<[Option<&[u8]>; N]>;

// src/ts.rs — rules.md "Timestamps".
/// F1 / F2 / F3 → epoch ms UTC. None = bad timestamp (never panics, incl. F3 overflow).
pub fn parse_ts(s: &[u8]) -> Option<i64>;
/// Days since 1970-01-01 for a UTC epoch-ms instant (floor division).
pub fn day_of(ms: i64) -> i32;
pub struct Window { pub start: i64, pub end: i64 }   // from manifest.json

// src/people.rs — rules.md "Linked IDs".
pub struct People { /* union-find */ }
impl People {
    pub fn build<'a>(pairs: impl Iterator<Item = (&'a [u8], &'a [u8])>) -> Self;
    /// The person's name: byte-wise smallest ID of its component; an unlinked ID names itself.
    pub fn name<'a>(&'a self, id: &'a [u8]) -> &'a [u8];
}

// src/output.rs — writes the two tables (one or more part-N.parquet files per folder).
pub struct UserDaily<'a> { pub user_id: &'a [u8], pub day: i32, pub sessions: i64, pub events: i64,
                           pub revenue_cents: i64, pub top_category: Option<&'a [u8]> }
pub struct CampaignDaily<'a> { pub campaign_id: &'a [u8], pub channel: &'a [u8], pub day: i32,
                               pub sessions: i64, pub purchasing_sessions: i64, pub revenue_cents: i64,
                               pub assisted_sessions: i64 }
pub fn write_user_daily(dir: &Path, rows: &[UserDaily]) -> io::Result<()>;
pub fn write_campaign_daily(dir: &Path, rows: &[CampaignDaily]) -> io::Result<()>;

// src/v1.rs — single-threaded pipeline: events → dedup → validate → late → people → sessions
//             → credit/assists → aggregate. `pub fn run(data: &Path, out: &Path) -> io::Result<()>`
// src/v2/…  — parallel pipeline (T20–T26), same output; falls back to v1::run on Overflow (D6, D13).
```

Data flow (V2): `mmap files → byte-range chunks → parse (parallel) → scatter by hash(event_id) → dedup + validate + late (per bucket) → map user → person, scatter by hash(person) → sort (person, ts) → sessionize + credit/assists against that person's clicks sorted (ts, click_id) → per-thread aggregates → merge → write Parquet`.

**The V2 module contracts (`src/id.rs`, `src/v2/{types,scan,parse,people,sessions,write,mod}.rs`) are in the §6 task cards T20–T26.** They replace the earlier V2 sketch.

### 5a. Optimization playbook (V2)

Mainstream engine techniques, mapped onto this workload. V2 tasks follow this; T13 measures each.

**What does *not* apply.** Predicate/projection pushdown into storage, zone maps, row-group stats and Bloom-filter skipping all need a format with metadata. CSV has none: every byte must be scanned to find row boundaries. Here "pushdown" means doing the least work per field *inside the parser*, not skipping input.

| # | Technique | Applied here | Task |
|---|---|---|---|
| O1 | **Fused single pass** (HyPer-style data-centric pipeline) | One loop per chunk: parse → encode → filter → scatter into buckets. No intermediate row tables; each input byte touched once | T22, T26 |
| O2 | **Morsel-driven parallelism** | Fixed byte-range chunks pulled from a shared queue (rayon), not one file per thread (largest file ≈ 16% of data) | T21 (D9) |
| O3 | **SIMD scanning** (simdjson / simdcsv) | Find `,` `"` `\n` 32–64 bytes at a time as bitmasks. Only 4 physical cores, so per-core parse speed decides the race | T13 |
| O4 | **Early filtering, bounded by dedup** | *Clicks*: drop at parse time (bad campaign, out of window, empty user/id); they need no dedup. *Events*: dedup comes before validation, so a row that fails validation still takes part in dedup, but only its `(event_id, ingest_ts, invalid)` matters; leave its other fields zero | T22 |
| O5 | **Dictionary encoding at parse** | category → `u16` index (D17), campaign → `u16` (≤ 2000 + `organic`), event_type → flag bits, price → `u32`. `products.csv` / `campaigns.csv` are tiny lookup tables: a lookup at parse, no join | T22 |
| O6 | **Late materialization** | IDs stay packed `u128` (D6) end to end; strings are rebuilt only when writing Parquet | T20, T25 |
| O7 | **Compact records** | D13: `ingest_ts` as exact `i32` seconds (fallback if out of range), `ts` as `u32` ms offsets, `EvRaw` 48 B, `Ev` 32 B | T22, T26 |
| O8 | **Radix partitioning** (DuckDB / HyPer joins) | Scatter into 2^k cache-sized buckets (k ≈ 8–10, per-worker buffers), then hash inside each: dedup by `event_id`, then re-partition by person | T26 (D8, D14) |
| O9 | **Radix sort** | (person, ts) per bucket as one `u64` key (person index within bucket + ts offset); LSD radix sort, typically 2–4× faster than `sort_unstable` at this size | T13 (T24 starts with `sort_unstable`) |
| O10 | **Partial / pre-aggregation** | `campaign_daily`: fixed per-worker array `[campaign_idx][day]`, summed at the end. `user_daily`: rows come out of the per-person walk in order, no hash table | T24 (D16) |
| O11 | **Early read-ahead** | At startup, `madvise(MADV_WILLNEED)` on every mmapped file so the kernel reads ahead while threads parse. Only matters if the scorer's disk is cold; cheap either way | T21 |
| O12 | **No allocation in hot loops** | Pre-sized per-worker buckets and buffers, reused across chunks; `mimalloc` | T21–T26 |

**Deliberately not done** (decided; measure in T13 only if profiling points there):
- **Output compression:** ~7M rows ≈ 150 MB uncompressed. Snappy/zstd costs more CPU than the write saves, and the 50 GB space limit is never close.
- **Parquet dictionary encoding:** may help `channel` / `campaign_id` / `top_category`, but those columns are small. T13 A/B only.
- **Deferring `ts` parse for duplicate copies:** ~97% of events are unique, so nearly every copy is the kept one. Deferring turns one sequential pass into random reads back into the file.
- **Spilling:** working set ~3–6 GB vs 24 GB. Never spill.
- **Cross-run caching or dataset fingerprinting:** every run is a fresh container on new data, and rules.md says nothing about a dataset can be hard-coded.

**Expected order of payoff:** O1+O2 (fused, morsel-parallel parse) > O3 (SIMD) > O7+O5 (compact, encoded records) > O9 (radix sort) > O11 (read-ahead, insurance for a cold disk).

---

## 6. Tasks

### Status

| Done | Result |
|---|---|
| H1, T00–T06, H2 | **V1 scored 40.95 s on hidden 20M, exact match.** Local native 36.2 s → scorer ≈ 1.13× local. |
| V1 merge | Two V1s were built in parallel (PR #1, and one on `main`). Both passed G1, with identical output on p20. The merge keeps **PR #1's code** (36 s vs 50 s at 20M; 50 vs 33 unit tests; it's the submitted SHA). The other V1 stays in history (`ce9edff`). |
| T20–T26 (V2.0) | Implemented on branch `v2`. **p20: ~2.6–2.8 s on 4 cores × 2 threads locally** (`NCPU=8 taskset -c 0-3,8-11`), vs V1 36 s. G1 passes at default, `SHOPT_CHUNK_BYTES=4096`, and `SHOPT_BUCKET_BITS=1 SHOPT_CHUNK_BYTES=100`; G4 `MATCH` vs V1 on p20; 2M seeds 41–43 match; forced V1 fallback (16-byte ID) matches the reference. Phases at p20: links+events parse 1.1 s, clicks 0.26 s, dedup 0.65 s, sessions 0.4 s, write 0.12 s. Peak RSS 5.5 GB (incl. 1.8 GB mapped input). `SHOPT_BUCKET_BITS` 8 vs 10 and 8 vs 32 MiB chunks: no measurable difference. |
| T07–T12 | **Superseded** by the Submission #2 cards below (T20–T28). T13–T15 still apply after #2. |
| Pass 1 (S3-0 to S3-7) | Merged up to `e6f1279`. **#3 scored 2.51 s on hidden 20M** (local 2.27 s, so scorer ≈ 1.1× local still holds). S3-8 (huge-page A/B) has no commit or record. 60M, measured Sat 20:15 ET, warm cache: **7.77 s local**. Phases: links+events 2.81 s, clicks 0.53 s, dedup **2.71 s (4.0× of 20M)**, sessions 1.41 s, write 0.30 s. Peak anon 7.4 GB (≤ 10 GB ✓), VmHWM 12.5 GB. A cold first run took 11.4 s. S3-9's other checks (self-checks, 5M seeds) aren't recorded. |

### Where V1's time goes (20M, local, single thread)

| Phase | ms | Why |
|---|---|---|
| read_inputs | 1,828 | `fs::read` copies 1.8 GB on one thread |
| lookups (links) | 477 | slice-keyed union-find |
| dedup | 13,780 | parse + 20M inserts into one big `HashMap<&[u8], EventRecord>` (cache misses) |
| validate | 10,900 | walks that map + `people.name` lookup + `HashMap<person, Vec>` pushes (random access) |
| clicks | 3,854 | parse + per-person map pushes |
| sessions | 4,102 | per-person sort + slice-keyed campaign maps |
| write | 702 | |
| **total** | **36,156** | peak RSS 6.4 GB (→ ~19 GB at 60M: OOM risk) |

Almost all of it is one thread doing random-access hash-map work on fat slice keys. V2 removes that with parallelism, fixed-size integer records, and partitioning so every hash table fits in cache.

### Submission #2 — V2.0: parallel, packed, compact

**Goal:** `data/p20` ≤ 2 s native locally (≈ 2.3 s on the scorer), output identical to V1, peak RSS ≤ 8 GB at 60M.

**Pipeline (fixed by D11–D17):**

```
P0 lookups   products → category index (u16); campaigns → campaign index (u16);
             links → PersonMap (parallel parse, serial union-find on dense indices)
P1 parse     event chunks (D9) → EvRaw, scattered into 2^B buckets by bucket(event_id)
             click chunks      → validated, person-mapped Ck, scattered by bucket(person)
P2 dedup     per event bucket, in parallel: keep the min-ingest copy per event_id;
             if it is VALID → person = PersonMap(user) → Ev, scattered by bucket(person)
P3 sessions  per person bucket, in parallel: sort Ev by (person, ts), Ck by (person, ts, click_id);
             sessionize, credit, assists; emit UserRow runs in-stream; campaign counts into
             per-thread dense [campaign][day] arrays
P4 write     user_daily: one Parquet file per worker, in parallel; campaign_daily: merge arrays → one file
```

Scatter = each worker pushes into its own `Vec` per bucket (`workers × 2^B` vectors); bucket `b` is the list of those per-worker slices. No locks anywhere.

**Record layouts (D13):** store `u128` as `[u64; 2]` so records align to 8.

```rust
pub struct EvRaw { id: u128, user: u128, ingest_s: i32, ts: u32, price: u32, cat: u16, flags: u8 } // 48 B
// ingest_s = exact seconds since window_start (D13). Invalid copies: only id, ingest_s, flags matter (O4)
// ts = ms since window_start (only meaningful when VALID); flags: VALID | PURCHASE
pub struct Ev    { person: u128, ts: u32, price: u32, cat: u16, purchase: bool }               // 32 B
pub struct Ck    { person: u128, click_id: u128, ts: u32, campaign: u16 }                      // ≤ 40 B
// Ck.ts = ms since (window_start − 7 d)
```

Memory at 60M: EvRaw ≈ 3.0 GB + Ev ≈ 1.9 GB + Ck ≈ 0.6 GB + mmapped input (page cache).

### Dependency graph (Submission #2)

```
Wave 1 (all parallel):  T20 id ─┐
                        T21 scan ┤
                        T22 parse┼── Wave 2: T26 orchestrate ── T27 perf pass ── H5 submit V2.0
                        T23 people┤
                        T24 sessions┤
                        T25 write ─┤
                        T28 bench ─┘
```

Wave 1 tasks are pure functions with unit tests and don't depend on each other's code, only on the contracts below. Shared types (`EvRaw`, `Ev`, `Ck`, `Ctx`, `UserRow`, `Overflow`) live in `src/v2/types.rs`. **The orchestrator writes that file first (≈ 15 min) from the layouts above, before spawning Wave 1.**

### Task cards (Submission #2)

---

**T20 — Packed IDs** · Scope: `src/id.rs` · Depends: `types.rs`
- Contract: `pub fn pack(b: &[u8]) -> Option<u128>` (None if > 15 bytes) · `pub fn unpack(v: u128, buf: &mut [u8; 15]) -> &[u8]` · `pub fn bucket(v: u128, bits: u32) -> usize` (strong 64-bit mix of both halves, take the top `bits`).
- Accept: `cargo test id`, with a property test over random byte strings ≤ 15 bytes (incl. `0x00`, spaces, upper case, prefixes) that `pack(a) < pack(b) ⇔ a < b` byte-wise and `pack` is injective. `U1 < u1 < "u1 " < u10 < u2` explicitly. Bucket balance: 1M sequential IDs spread within ±10% across 256 buckets.

---

**T21 — Chunking and mmap** · Scope: `src/v2/scan.rs` · Depends: —
- Contract: `pub fn map_dir(dir: &Path) -> io::Result<Vec<Mmap>>` (every `*.csv`; **zero-length files are skipped, since mmap of an empty file fails**) · `pub fn chunks<'a>(files: &'a [Mmap], target: usize) -> Vec<&'a [u8]>`. Each chunk holds only whole data lines: the header is excluded, a chunk ends just after a `\n` or at EOF, and no line is split or repeated. Use `target` from `SHOPT_CHUNK_BYTES` (default 32 MiB). Call `madvise(MADV_WILLNEED)` on each map (O11).
- Accept: `cargo test scan`. For chunk sizes 1, 7, 64, 4096 and the default, the concatenation of `csv::data_lines` over all chunks equals `csv::data_lines` over each whole file. Cover: header-only file, file with no `\n` at all, last line without `\n`, `\r\n` lines, an empty line in the middle, a 0-byte file.

---

**T22 — Row parsers** · Scope: `src/v2/parse.rs`, plus `pub(crate)` on `v1::parse_price` and `v1::kind_of` · Depends: `types.rs`, T20 (`pack` signature only)
- Contract: `pub enum Parsed<T> { Skip, Overflow, Rec(T) }` · `pub fn event(line: &[u8], ctx: &Ctx) -> Parsed<EvRaw>` · `pub fn click(line: &[u8], ctx: &Ctx) -> Parsed<(u128 /*user*/, Ck /*person unset*/)>`.
- Rules: `Skip` = malformed line or empty `event_id`/`click_id`, or for clicks any failed validation (no click dedup, D7). `Overflow` = any ID that must be packed is > 15 bytes. EvRaw `VALID` = every per-copy check from rules.md "Events" steps 3–4 (user, ts in window, type, product, purchase price, **not late**). Unknown product → the `unknown` category index. `Overflow` also when `ingest_ts` doesn't fit D13's `i32` seconds. Apply §5a O4, O5, O7.
- Accept: `cargo test parse`, with cases for E5 (invalid copy still `Rec` with `VALID` unset), E6 (exactly 24 h kept, +1 s dropped), E10, E12, E13, E17, E18, E19, E20, A6, A10 (click window edges), and a product absent from `products.csv`.

---

**T23 — PersonMap** · Scope: `src/v2/people.rs` · Depends: T20, T21 (signatures only)
- Contract: `pub fn build(links: &[Mmap], chunk: usize) -> Result<PersonMap, Overflow>` · `impl PersonMap { pub fn person(&self, user: u128) -> u128 }` (an unlinked ID maps to itself).
- How: parse link lines in parallel → `Vec<(u128, u128)>`; map IDs to dense `u32` with one hash map; iterative union-find with path halving; name = min packed ID per root; final `HashMap<u128, u128>` for linked IDs only.
- Accept: `cargo test v2::people`, with L1–L3 behaviour, a 400-link chain, and a randomized test (≥ 1,000 random link sets) giving the same names as `people::People`.

---

**T24 — Session kernel** · Scope: `src/v2/sessions.rs` · Depends: `types.rs`
- Contract: `pub fn run_bucket(evs: &mut [Ev], cks: &mut [Ck], ctx: &Ctx, acc: &mut Acc)` · `pub struct Acc { pub users: Vec<UserRow>, pub camp: Vec<[i64; 4]> /* (n_campaigns + 1 organic) × n_days: sessions, purchasing, revenue, assisted */, scratch… }` · `pub struct UserRow { person: u128, day: i32, sessions: i64, events: i64, revenue: i64, top_cat: Option<u16> }`.
- How: sort (`sort_unstable`; radix sort O9 is a T13 follow-up); sessionize (gap between distinct ts > 30 min); credit = first click of the latest-`ts` group in `[start − 7 d, start]`; assists via a per-`Acc` count array over campaign indices plus an active list with swap-remove (D15); consecutive sessions of the same person and day fold into one `UserRow` (D16); `top_cat` = max cents, ties → smallest index (D17).
- Accept: `cargo test v2::sessions`, porting A1–A12, E1–E3, E8, E9, E14, E16, L1 and L4 at the record level, plus a randomized test (≥ 500 cases, ≤ 50 events and ≤ 30 clicks each) against a naive O(n²) implementation written in the test module straight from rules.md.

---

**T25 — Writer** · Scope: `src/v2/write.rs` (reuse `output.rs` schema code; may make helpers `pub(crate)`) · Depends: `types.rs`, T20 (`unpack`)
- Contract: `pub fn write_users(dir: &Path, part: usize, rows: &[UserRow], ctx: &Ctx) -> io::Result<()>` (writes `part-{part}.parquet`) · `pub fn write_campaigns(dir: &Path, camp: &[[i64; 4]], ctx: &Ctx) -> io::Result<()>` (rows where credited sessions > 0 or assisted > 0; organic has assisted 0).
- Accept: `cargo test v2::write`, with a round-trip read via the parquet reader, and the schema (names, physical and logical types, nullability) identical to V1's output files.

---

**T28 — Bench and oracle script** · Scope: `scripts/bench.sh` (new), `Makefile` (add `bench` and `oracle` targets) · Depends: —
- Contract: `make oracle DATA=data/p20` runs `SHOPT_ENGINE=v1 target/release/shoptimizer` once and caches its output in `DATA.v1out/` (skips if present). `make bench DATA=data/p20 [RUNS=3] [CAND=…]` runs the candidate natively `RUNS` times into a fresh `out-bench/`, prints each wall time, the median, and the peak RSS, then runs `scripts/compare.py out-bench DATA.v1out` and prints `MATCH`/`DIFFER`. Until T26 adds `SHOPT_ENGINE`, V1 is the default engine, so `oracle` works today.
- Accept: `make oracle DATA=data/p20 && make bench DATA=data/p20` prints 3 times, a median, peak RSS and `MATCH`.

---

**T26 — Orchestration and fallback** · Scope: `src/v2/mod.rs`, `src/main.rs` · Depends: T20–T25
- Do: P0–P4 on a rayon pool sized `NCPU`; `[phase]` timings to stderr; bucket bits from `SHOPT_BUCKET_BITS` (default 8). **Fall back to `v1::run`** on any `Overflow`, or if `window_end − window_start + 7 d` doesn't fit in `u32` ms. `SHOPT_ENGINE=v1|v2` forces an engine (default v2).
- Accept: **G1** (default chunk size and `SHOPT_CHUNK_BYTES=4096`, plus `SHOPT_BUCKET_BITS=1`), **G2**, **G4** on `data/p20`. Report the p20 phase timings and the peak RSS.

---

**T27 — Pre-submit perf pass** · Scope: `src/v2/**` · Depends: T26 · **Time-box: 1 hour**
- Do: `perf record` on `data/p20`. Fix only the top hotspot, and only if the fix is local and obviously safe. Otherwise note it for T13.
- Accept: G1 + G4 still pass; p20 median no worse than T26's.

---

**H5 — Submit V2.0** · Human · §8 protocol. Record the time; if anything differs on hidden data, resubmit the last good SHA and stop to investigate.

---

### Submissions #3 and #4: two build passes

Source: `research/perf/SYNTHESIS.md` (ranked list), with details in `research/perf/01`–`12` and `research/perf/deep-research.md`. Base: `main` after `8cb84b0` (V2.0 code = `bf0d6d5`).

**Rules for both passes**
- One card = one branch `s3-<n>-<slug>` / `s4-<n>-<slug>`, merged in the order given. Cards in the same wave touch different files and can run in parallel.
- Every card that changes `src/` passes **G1 in all configs** (§7: default, `SHOPT_CHUNK_BYTES=4096`, `SHOPT_BUCKET_BITS=1 SHOPT_CHUNK_BYTES=100`, plus any new knob set to its smallest value) and **G4**. Speed cards also report `make bench DATA=data/p20` before and after (G3).
- **V1 behaviour never changes** (D11). Shared code (`csv.rs`, `ts.rs`, `output.rs`, `v1.rs`) may only get changes that are byte-identical in output. New fast paths live under `src/v2/` with a differential test against the shared original.
- Benchmarks run **serially**, with nothing else running, under `NCPU=8 taskset -c 0-3,8-11` (4 cores × 2 threads, like the scorer).
- Log a row in §8 for every submission.

**Plan corrections that apply now**
- **D3 allow-list:** add `memchr` (pure Rust, AVX2 runtime dispatch, builds in seconds) for S3-4.
- **T14 (v3/v4 A/B): dropped.** Both research efforts say keep `x86-64-v3`.
- **T15 gate:** measure **`RssAnon` / anonymous peak**, not RSS. RSS includes the 5.5 GB mapped input, which can't cause an OOM kill. New accept line: peak anon ≤ 10 GB at 60M.
- **D14:** `SHOPT_BUCKET_BITS` is replaced by `SHOPT_DEDUP_BITS` and `SHOPT_PERSON_BITS` in S4-2. Raising the single knob regresses today (nb² growing `Vec`s).

---

### Pass 1: Submission #3 (low risk, ~8–12 h, target ≈ 1–2 s off at 60M)

```
Wave A (parallel):  S3-0 bench ─┐
                    S3-1 cat    │
                    S3-2 parquet├─ Wave B: S3-5 scatter+presize ── S3-6 largest-first ── Wave C: S3-4 scan ── S3-8 huge-page A/B ── S3-9 60M check ── H6 submit
                    S3-3 exit   │
                    S3-7 stats ─┘
```

---

**S3-0 — Bench and oracle targets** (was T28) · Scope: `scripts/bench.sh` (new), `Makefile` · Depends: — · 1 h
- `make oracle DATA=d` runs `SHOPT_ENGINE=v1 target/release/shoptimizer d d.v1out` once (skips if `d.v1out/` exists).
- `make bench DATA=d [RUNS=3] [CAND=…]` runs the candidate `RUNS` times under `NCPU=8 taskset -c 0-3,8-11` into a fresh `out-bench/`. It prints each run's `[phase]` lines and wall time, the median wall time and peak RSS, then runs `scripts/compare.py out-bench d.v1out` and prints `MATCH`/`DIFFER`.
- Accept: `make oracle DATA=data/p20 && make bench DATA=data/p20` prints 3 runs, a median and `MATCH`. Record the median as the **#3 baseline**.

---

**S3-1 — Category lookup only for purchases** · Scope: `src/v2/parse.rs` (+ its tests) · Depends: — · 1–2 h · est. 250–400 ms
- In `parse::event` (`parse.rs:83`), do the `ctx.products` lookup only when `purchase`. Non-purchases set `cat = ctx.unknown_cat` (never read: `Ev.cat` is only used under `if ev.purchase` in `sessions.rs`).
- **Keep every validity check unchanged:** a non-empty `product_id` is still required for all event types.
- Accept: G1 + G4. Plus a unit test that a `view` with an unknown product is still `VALID`, and that a `purchase` gets the right category, including `unknown`.
- Traps: E9 (unknown product wins `top_category`), E13 (empty product invalid).

---

**S3-2 — Parquet writer settings** · Scope: `src/output.rs` (`open_writer`, `output.rs:85-95`) · Depends: — · 1 h · est. 60–130 ms (measured locally: single-thread encode 124 → 52 ms at 2.2M rows)
- In `WriterProperties`:
  - dictionary **off** for every column except `top_category`, `channel` and `campaign_id`;
  - statistics **off** (`EnabledStatistics::None`);
  - offset/column index off, if parquet 60 exposes it.

  Check the exact builder method names in the parquet 60.x docs; don't guess.
- Metadata-only: schema, types, nullability and `DATE` are unchanged. V1 gets the same writer, and its output stays identical.
- Accept: G1 + G4. Plus a round-trip test reading the file with `SerializedFileReader`, which confirms the schema is unchanged and that zero-row tables still carry it.

---

**S3-3 — Skip teardown** · Scope: `src/v2/mod.rs`, `src/main.rs` · Depends: — · 1 h · est. 50–300 ms
- Large buffers today are freed on the critical path: `drop(ev_parts)` at `mod.rs:166`, `drop(ev_out)`/`drop(ck_parts)` at `:192-193`, and everything else when `run_v2` returns.
- Change each mid-run `drop(x)` to `std::thread::spawn(move || drop(x))`, which frees in the background while the next phase runs.
- At the end of a successful V2 run, after both Parquet writers have returned (files closed), flush stderr and call `std::process::exit(0)` from `main`, so `acc`, the mmaps and the maps are never dropped. V1 and the fallback path are unchanged.
- Accept: G1 + G4. Report the wall-time delta, and peak anon memory from S3-7, since background frees may raise the peak.

---

**S3-7 — I/O and memory instrumentation** · Scope: `src/v2/mod.rs` (`phase()`), `src/main.rs` · Depends: — · 2 h · est. 0 ms (enables S3-8 and S3-9)
- When `SHOPT_IO_STATS=1`, each `[phase]` line also prints:
  - `minflt` and `majflt` deltas (from `/proc/self/stat`);
  - `RssAnon` and `RssFile` (from `/proc/self/status`);
  - and at the end, `VmHWM`.
- stderr only. Off by default, with zero cost when off.
- Accept: G1. One p20 run with stats on, pasted into the PR description.

---

**S3-5 — One accumulator per worker, pre-sized buffers** · Scope: `src/v2/mod.rs` (P1 folds `:76-121`, P2 output `:149`) · Depends: S3-0, S3-3 merged · 3–4 h · est. 200–400 ms
- **P1 (events and clicks).** Replace `par_iter().fold(|| vec![Vec::new(); nb], …)`, which creates one accumulator per rayon *split*, with exactly one accumulator per worker:
  - spawn `rayon::current_num_threads()` tasks;
  - each task owns one `Vec<Vec<_>>` and pulls chunks from a shared `AtomicUsize` index until they run out (dynamic load balancing, one bucket set per worker).
- **Pre-size** each worker's bucket `Vec` to `est_rows / workers / nb × 1.15`, where `est_rows = total input bytes / 70` (events) or `/ 45` (clicks). Under-estimates still grow normally; no correctness dependence.
- **P2 output (`:149`).** Each dedup task first counts survivors per person bucket, then allocates each output `Vec` with its exact length, so nothing doubles.
- Accept: G1 (all configs) + G4. Report `links+events`, `clicks` and `dedup` phase times and peak anon.
- Traps: none semantic. Row order and file count are free (`rules.md` Outputs).

---

**S3-6 — Largest buckets first** · Scope: `src/v2/mod.rs` (P3 `:170-190`) · Depends: S3-5 · 0.5 h · est. 20–60 ms
- In P3, iterate person buckets in descending order of total `Ev` count, so the whale bucket starts first instead of finishing last.
- Accept: G1 + G4.

---

**S3-4 — Cheap scan wins** · Scope: `src/v2/scan.rs` (`find_newline`, `:36`), new `src/v2/split.rs`, `src/v2/parse.rs`, `src/v2/people.rs`, `Cargo.toml` (`memchr`) · Depends: S3-1 merged (both touch `parse.rs`) · 4–6 h · est. 400–900 ms
- Replace `find_newline` with `memchr::memchr(b'\n', …)`.
- Add `split::<N>` in `src/v2/split.rs`: a **single-pass** splitter with exactly the semantics of `csv::split`. Today's `csv::split` scans for commas, then `field()` rescans each field for quotes 1–2 more times. The new one walks bytes once: it finds commas (or uses `memchr` for `,`) and checks the "quote only as first and last byte" rule in the same pass. V2 (`parse.rs`, `people.rs`) calls it; V1 keeps `csv::split`.
- Accept:
  - A **differential test**: `v2::split::split::<N>` equals `csv::split::<N>` for every string up to length 8 over the alphabet `{',', '"', 'a', ' ', '\r'}` (N = 2, 4, 7), plus 1M random lines up to 120 bytes.
  - G1 (all configs) + G4.
- Traps: empty line, wrong field count, `""` → null, `"a"b`, `a"b`, `"`, CR stays data (E13, E15, E19, E20, L3).

---

**S3-8 — Huge pages A/B** · Scope: `Dockerfile` (`ENV` only) or `src/main.rs` (mimalloc option) · Depends: S3-5, S3-7 · 1 h · est. 0–500 ms
- Check `/sys/kernel/mm/transparent_hugepage/enabled` locally and note it.
- A/B `make bench` with and without `MIMALLOC_ALLOW_LARGE_OS_PAGES=1`. Confirm the variable name against libmimalloc-sys 0.1.49 (mimalloc 2.x docs).
- Adopt the variable (as a Dockerfile `ENV`) only if the p20 median improves ≥ 3% across 2 × 3 runs. Otherwise record the result and drop it.
- Accept: G4 under the adopted setting.

---

**S3-9 — 60M check (revised T15)** · Scope: none unless a fix is needed · Depends: all S3 cards · 1 h
- Generate `data/p60`:

  ```
  .tools/generate --seed 9 --events 60000000 --clicks 15000000 --output data/p60
  ```

- Run V2 once natively with `SHOPT_IO_STATS=1` under `NCPU=8 taskset -c 0-3,8-11`.
- Check the §7 self-checks on the output:
  - summed `sessions` and `revenue_cents` are equal across the two tables;
  - summed `assisted_sessions` ≥ credited non-organic sessions.
- Run G1's 2M sweep at `--events 5000000 --clicks 1250000` on seeds 51–53.
- Accept: peak anon ≤ 10 GB; self-checks hold; 5M seeds match; record the 60M wall time.

---

**H6 — Submit #3** · Human · §8 protocol (G2 in Docker first). Record the practice time.

---

### Round 2: research, then Pass 2′ (Sat night → Sun 09:00 freeze)

**Why:** #3 is 2.51 s on 20M, but the final is 60M, where we run **~7.8 s locally (~8.5–9 s on the scorer)**, and dedup scales worse than linearly. Pass 2 below was sized from 20M data. Round 2 re-checks it at 60M, and looks for bigger architectural cuts (a global atomic dedup winner table, a full AVX2 scanner, fewer bytes moved) before we spend the night building. The practice leader (0.27 s) is still most likely dataset shortcuts (§9). The `roofline` track will say how close an honest pipeline can get.

**Brief:** `research/perf2/README.md`. Two halves in parallel: a pi fan-out (one researcher per track; the track count comes from the 60M profile, not fixed) and /deep-research in the synthesis session. Output goes to `research/perf2/`. Every report ends with a ready-to-build card and says whether it fits before the freeze.

**Schedule (ET, times are latest-allowed):**

| By | Step | Who | Exit criterion |
|---|---|---|---|
| Sat 22:15 | **R2-0** 60M profile (serial) + **R2-1** fan-out + /deep-research | pi orchestrator; synthesis session | `research/perf2/00-profile.md`, `INDEX.md`, one report per track, `deep-research.md` |
| Sat 22:45 | **R2-2** synthesis → `research/perf2/SYNTHESIS.md` + rewrite of the Pass 2 cards below as **Pass 2′** | synthesis session | Each Pass 2 card is kept, changed or replaced. New cards are in §6 format, with a dependency graph and waves. |
| Sun 04:30 | **Pass 2′ build** (implementer session, card by card, gates as in "Rules for both passes") | implementer | Every card merged with G1 (all configs) + G4 + bench |
| Sun 05:30 | **S4-5** 60M check + G2 Docker | implementer + human | Anon peak ≤ 10 GB, 5M seeds 51–53 match, 60M time recorded |
| Sun 06:00 | **H7** submit #4 | human | Recorded in §8 |
| Sun 08:30 | Fix-only buffer. Optionally one small extra card if #4 is clean. | | |
| Sun 09:00 | **H4** freeze | human | |

**Research closed:** R2-2 has landed; the research-only restrictions applied to the fan-out, and implementation order is now set by Pass 2′ below. Nothing from Pass 2 (old) may start except through its renumbered card.

**Decision rules for the synthesis:**
- Rank by ms saved **at 60M** per hour, not at 20M.
- A design that removes a whole scatter or phase beats tuning that phase, if it fits in ≤ 6 h with its tests.
- Nothing ships that lacks an exact fallback for rows the fast path rejects (D11, rule 3 of the brief).
- Keep total Pass 2′ effort ≤ 9 h of implementer time. Anything bigger is split, and only its first half is built.

### Pass 2′: Submission #4 (renumbered by the round-2 synthesis R2-2)

**Source:** `research/perf2/SYNTHESIS.md` (ranked decisions and rejections), per-track detail in `research/perf2/01`–`08`, external evidence in `research/perf2/deep-research.md`. Base: `main` at `e6f1279` (#3). Hard cap: **9 implementer-hours including gates**. Build in wave order; one branch per card.

```
S4-0 (done) ─ W1: S4-3 exact-key dedup ─ W2: S4-1 flat scatter ─ W3: S4-2 split bits → S4-4 screen ─ W4: S4-5 validate ─ H7
```

**Wave rules (supersede the old card order).** Every card merges through G1 in all configs + G4 + G3 before/after, plus a pinned serial **60M wall** comparison against its card's parent: same host, warm-cache procedure, `NCPU=8 taskset -c 0-3,8-11`, nothing else running. Confirm any promising delta with ≥ 3 runs per version in alternating order; keep the parent when ranges overlap. One card at a time. Tiny-knob configs are correctness-only (G1 data), never 60M.

**S4-0 — Profile** · Done (R2-0). `research/perf2/00-profile.md` replaces the p20-only profile: 60M phase table, top-40 symbols, faults.

**W1 · S4-3 — Exact-key compact dedup** · Scope: `src/v2/mod.rs` (P2), focused tests; optional private `src/v2/dedup.rs` · Depends: S4-0 · **cap 3 h** incl. gates · hypothesis 250–400 ms at 60M
- Replace the bucket-local `HashMap<u128, EvRaw>` with an exact-key map from the full event id to a dense winner index. Append a payload only on vacancy; on a smaller `ingest_s`, overwrite the existing winner in place. Checked indices (`usize`); never pack or truncate a row index or worker id.
- Keep P2's output representation in this card. Semantics are mechanically identical: min `ingest_s` wins, equal-ingest copies are interchangeable, invalid survivors are still dropped after dedup (E5), the winner's user is resolved to a person exactly once (E4). Keep the current reducer as a test oracle.
- Accept: randomized shuffled cross-chunk duplicates vs the old reducer; forced-collision ids; earlier-invalid/later-valid and earlier-different-user cases; equal-ingest; empty buckets. Plus the common gates and the 60M wall/anon comparison. A P2 win that regresses total wall is not a win.

**W2 · S4-1 — Flat counted person scatter** · Scope: `src/v2/mod.rs` (P2 output, P3 gather) · Depends: S4-0, and W1's accepted state · **cap 2.5 h** · hypothesis 100–300 ms
- Reuse the existing valid-winner count pass (`mod.rs:238-247`) and the person cached there; prefix-sum into checked offsets per dedup task; fill **one** contiguous `Vec<Ev>` per task with disjoint per-person-bucket ranges. P3 concatenates the corresponding slices (empty ones included) and sorts as today. Clicks keep their S3-5 buckets.
- Accept: multiset equality vs the old nested scatter on empty, single-bucket and heavily skewed inputs; offset ends equal final counts; common gates; 60M wall and anon.

**W3 · S4-2 — Split bucket controls** · Scope: `src/v2/mod.rs`, D14, tests · Depends: S4-1 · **cap 1 h**, plus the 0.5 h S4-4 screen
- `SHOPT_DEDUP_BITS` controls the P1 event scatter and P2 tasks; `SHOPT_PERSON_BITS` controls P2 person output, click scatter and P3. Each overrides its side of the legacy `SHOPT_BUCKET_BITS` shorthand, which keeps working when set alone. Preserve 0-bit (single bucket) behavior; bound or reject an absurd requested product before allocation — never omit buckets.
- Accept: asymmetric (1,3)/(3,1), shorthand precedence, both-at-1 tiny config (§7 updated), G1 + G4 at the chosen defaults.

**W3 · S4-4 — Partition screen, not a sub-sort** · Scope: experiment only; only S4-2 defaults may change · **cap 0.5 h**
- At p60, screen (dedup, person) = (8,8) → (10,8) → (10,11), one axis at a time; confirm the best against (8,8) by the common comparison protocol. Keep the old defaults when noisy. Report dedup, sessions, total and anon. Do not infer L2 fit from mean bucket size. The original S4-4 sub-partition code is dropped (synthesis: its per-bucket rank pass costs more than it saves).

**W4 · S4-5 — Validate the final candidate** · Scope: none unless a fix is needed · Depends: last accepted wave · **reserve 2 h**
- The S3-9 procedure on the #4 code: 60M self-checks, sampled peak anon ≤ 10 GB (a lower bound; report VmHWM separately), 5M seeds 51–53 vs the reference, native wall and cache conditions recorded, G2 Docker (36/36, dev MATCH < 30 s), G1/G3/G4 re-confirmed on the selected source.

**Budget and cut line:** 3 + 2.5 + 1 + 0.5 + 2 = 9 h. Card caps are stop rules, not grounds for skipping tests. Reserve validation before opening any new card. Stop optimizing by Sun 04:30 ET to hold the 05:30 validation and 06:00 submission slots; if gates overrun, drop W3 first, then unfinished W2, and never half-land the knob split. If nothing beats #3 measurably, ship #3.

**H7 — Submit #4** · Human · §8 protocol.

### Hybrid: Pass 2′ is now two lines (chosen Sat ~21:45 ET)

Two syntheses exist:
- `research/perf2/SYNTHESIS.md`: conservative. It is the W1–W4 cards above.
- `research/perf2/SYNTHESIS-alt.md`: adds the thin-tuple dedup and a huge-page re-test. Its cards are in `research/perf2/PASS2-ALT.md`.

The hybrid runs both, as two lines that never share a P2 rewrite.

```
A0 bench@60M + lock (first, ~45 min; both lines need it)
├─ Line 1 → #4 (safe):     W1 ─ W2 ─ W3 ─ W4 ─ H7          [cards above, unchanged]
│    └─ side worktree:     A1 sys-time diagnosis ─ A2 huge-page alloc   → merged into Line 1 before W4, only if accepted
└─ Line 2 → #5 (stretch):  B1 thin dedup (from e6f1279) ─ B2 bits A/B ─ B3 60M check + G2 ─ H8
                           #5 = e6f1279 + A0 + (A2 if accepted) + B1 + B2. W1–W3 are NOT in #5: B1 replaces P2.
```

**Cards:**
- **A0, A1, A2:** as written in `research/perf2/PASS2-ALT.md`, run in a side worktree.
- **A2's evidence:** it is adopted only on its result under THP `madvise`. The S3-8 "negative A/B" doesn't count against it, because it ran under THP `always`, which hides the effect (see `SYNTHESIS-alt.md`).
- **B1–B3:** as written in `PASS2-ALT.md`. B1's own `SHOPT_DEDUP_BITS` / `SHOPT_PERSON_BITS` stand in for W3 on Line 2.
- **Optional, if an implementer is idle:** A3 (timestamps, `07`) and A4 (background unmap). Either one goes into whichever line is ahead.

**Benchmarks:** each line's cards may run in parallel worktrees. `make bench` holds `.bench.lock` (A0), so timed runs never overlap. Every benchmark follows the machine-state rule: THP `madvise`, swap cleared, `free -g` recorded.

**Outcomes (Sat ~23:30 ET):** A0 merged to `main` (`739cea6`) with `data/p60.ref` created from `e6f1279` verified output; A1 merged to `main` (`267e0cd`, review clean); W1, W2, A3, W3 merged to `line1` (`e5a8d7c`); B1 merged to `line2` (`ce3e7da`) and B2 defaults (10, 10) adopted (`ffb7506`, winner of the 3-run confirmations: 7.516 s vs 8.053 s and 9.379 s). **A2 rejected** on its madvise A/B (see §8); the THP-`always` no-regression check was skipped (needs sudo to flip the mode) - the madvise evidence was decisive on its own. W4/B3 validation complete for both lines: G2 Docker, seeds 51-53, p60 MATCH, self-checks, sampled peak anon <= 10 GB (line1 7.40 GB, line2 6.35 GB). Dev host had swap pressure during the 60M medians (pswpin/out > 0), so treat them as +/-10%.

**Deadlines (ET):**

| Time | Milestone |
|---|---|
| Sat 22:45 | A0 merged; `data/p60.ref` created |
| Sun 02:00 | A2 decision (adopt or drop) |
| Sun 03:30 | B1 go/no-go: it passes G1 or Line 2 stops |
| Sun 04:30 | Both lines stop optimizing |
| Sun 05:30 | W4 and B3 validation done |
| ~Sun 06:00 | Submit #4, then #5 (3 per hour allows both) |

**H4 (rule change):** pick the build with the best **local 60M bench median** among accepted submissions that passed every gate. The 20M practice time breaks ties only.

---
### Not in these passes (decided)
Full AVX2 structural scanner (round-1 01; round-2 03 keeps only span-fast `split` as a fallback card), packed-`u64` radix sort, hand-written Parquet, PGO, AVX-512, fused streaming P1→P2, parallel union-find, fork-and-exit tricks (defeated by PID-namespace teardown).

**Round-2 rejections** (evidence in `research/perf2/SYNTHESIS.md`): global atomic winner table and two-pass parse (02); fingerprint-only slots without full-key verify; arena/THP experiments (01, 04 — S3-8 already rejected the env default and the host A/B was negative; **overridden by the hybrid:** A2 re-tests huge pages under THP `madvise`); click–dedup overlap and the person-probe memo (06, 08 — measurement-gated with uncertain expected value); timestamp fast paths (07 — deferred behind the dedup budget); original S4-4 sub-partition code (05).

**H4 — Final pick and freeze** · Human · Sun 09:00 AM ET
- Pick the fastest commit that passed G1 + G2 + G4, the latest 60M check, and an accepted practice run. Make sure it's pushed and the repo is public. No new submissions after freeze unless the current best is broken.

---

## 7. Gates

| Gate | Commands (with `C=target/release/shoptimizer`) | Pass |
|---|---|---|
| **G1 correctness** | `cargo test` · `make fixtures CAND=$C` · `make check-dev CAND=$C` · `make diff-seeds CAND=$C SEEDS=2-30` · `.venv/bin/python scripts/diff_seeds.py --seeds 41-43 --events 2000000 --clicks 500000 -- $C` | all tests pass, 36/36, `MATCH`, every seed matches |
| **G2 docker** | `make docker-fixtures` · `make run-dev` | 36/36, `MATCH`, dev run < 30 s |
| **G3 performance** | `make bench DATA=data/p20` vs the best recorded in §8 | median no worse than +5% (unless the change is a correctness fix) |
| **G4 big-data oracle** | `make oracle DATA=data/p20` (once) · `make bench DATA=data/p20` | `MATCH` against V1's cached output. V1 matched the hidden 20M set exactly, so it's the trusted oracle at sizes the Python reference can't handle. |

V2 tasks run G1 in every config: default, `SHOPT_CHUNK_BYTES=4096`, and `SHOPT_BUCKET_BITS=1 SHOPT_CHUNK_BYTES=100` (after S4-2: `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`).

---

## 8. Submission protocol and results log

Human only.

**The submit form** (track page → Submit) asks for:

| Field | Value |
|---|---|
| Public GitHub repository | `https://github.com/aandrx/shoptimizer` |
| Commit (optional) | **Always fill in the full 40-character SHA.** Left empty, it scores the newest commit on the default branch, which may not be the one you tested. |

**Form rules:**
- Quota shown as "N of 3 left this hour" (3/hour at last check; "quotas may be reduced at any time").
- The repo must be **public at submit time** with the `Dockerfile` at the top. It may go private again once that submission is scored.
- **Push before submitting.** They fetch from GitHub. A rejected entry (unfetchable commit, failed build, failed dev check) still uses one of the hour's slots.
- They keep a copy of every scored commit, and the final uses that copy.

**Pre-submit checklist** (all on the exact SHA being submitted):
1. G1 + G2 (§7) pass.
2. `git push` done; `git ls-remote origin` shows the SHA on a pushed branch.
3. Repo is public (the URL opens logged out).
4. Paste the full SHA (`git rev-parse HEAD`) into the form.
5. Record a row below right away, then update it when the result arrives.

Space submissions out so a failure can be fixed before the next slot.

| When (ET) | Commit | What | Practice time | Result / notes |
|---|---|---|---|---|
| Sat Nov 21 | `2d9b48067e9f4909dd9f426effa71fbcfd9c20b5` | V1, single-threaded | **40.95 s** | Accepted: exact match on hidden 20M. Local native 36.2 s, so scorer ≈ 1.13× local. |
| Sat Nov 21 | V2.0 `bf0d6d5…` (#2) | V2.0 parallel pipeline | not recorded | |
| Sat Nov 21 ~20:00 | `e6f1279…` (HEAD at submit time; **confirm the full SHA**) | #3, Pass 1 (S3-0 to S3-7) | **2.5088 s** | Accepted. Local p20 2.27 s, so ≈ 1.1×. Leader at the time: 0.2716 s. |
| Sat Nov 21 ~23:30 | `e5a8d7c` (branch `line1`, unpushed) | #4 candidate, Line 1 (W1 exact-key dedup + W2 flat scatter + A3 timestamps + W3 split knobs). A2 rejected | - | All gates green (G1 x3, G4, G2 Docker 0.668 s, seeds 51-53, p60 MATCH, self-checks). Local p60 median **6.616 s** (3 runs, tight), sampled peak anon 7.40 GB <= 10 GB. Not yet submitted (H7 is human). |
| Sat Nov 21 ~23:30 | `ffb7506` (branch `line2`, unpushed) | #5 candidate, Line 2 (B1 thin-tuple dedup + B2 defaults 10/10). Excludes W1-W3 by design | - | All gates green (G1 x5 configs incl. `wide` fallback, G4, G2 Docker 0.668 s, seeds 51-53, p60 MATCH, self-checks). Local p60 median **5.866 s** (3 runs, tight), sampled peak anon 6.35 GB <= 10 GB. Not yet submitted (H8 is human). Best 60M median. |
| Sat Nov 21 ~23:30 | `3fe9204` (branch `a2-hugealloc`, not merged) | A2 huge-page allocator - **rejected** | - | Failed acceptance under THP `madvise`: 10.801 s vs 9.506 s base median (needs >= 3% better); dedup 2.77->4.01 s with fallbacks/compaction. Registry-based ownership design is sound per review; kept for the record. |
| | | | | |

---

## 9. Reference data

### Measurements (local generator)

| | 1M (dev, seed 1) | 20M (seed 7) | 60M (estimate) |
|---|---|---|---|
| Total on disk | 92 MB | 1.8 GB | ~5.5 GB |
| Event row size | ~77 bytes | | |
| Event files | 30 | 143 | |
| Largest file | | 287 MB (~16% of data) | |
| Parallel read, page-cached | | 0.1–0.2 s | |
| Max ID length | 9 (product ≤ 6) | 9 (product ≤ 6) | |
| `user_daily` rows | 117,038 | 2,242,393 | ~7M |
| V2 after Pass 1, warm, 4c×2t | | 2.27 s | **7.77 s measured** (dedup 2.71 s; peak anon 7.4 GB) |

### Time budget, 60M on 8 cores, page-cached

| Phase | Target |
|---|---|
| Parse 5.5 GB | 0.7–1.4 s |
| Dedup ~64M | ~0.5 s |
| Sort, sessionize, attribute | 0.5–1 s |
| Parquet + container start | ~0.3 s |
| **Total** | **2–4 s** |

### Trap list (each must have a test somewhere)

- Dedup before validation; an invalid kept copy drops the event; a later valid copy does not rescue it (E5).
- The kept copy's user counts, even if later copies name another user (E4).
- Malformed lines are dropped before dedup and never become the kept copy.
- Only the first line of a file is the header; a later identical line is data (L3).
- F3: leading zeros fine; very short or > i64 → out of window, not malformed; never panic.
- Offsets `-00:00`, `+23:59` valid; `+24:00`, `+05:60`, `+0200` bad; Feb 29 only in leap years (windows can include 2028-02-29).
- `\r` is data (`1000\r` is a bad price); a final line without `\n` is a line; header-only files.
- Spaces and case are significant everywhere; upper-cased IDs sort first and can become a person's name.
- Equal timestamps share a session regardless of read order (E16); gap exactly 30:00.000 stays, 30:00.001 splits.
- Late rule: `ingest_ts − ts > 24 h` drops; exactly 24 h stays; `ingest_ts` before `ts` stays.
- Credit: latest `ts ≤ start` within 7 days inclusive; tie → **smallest** `click_id`; a click at exactly the start counts, 1 ms after doesn't.
- Click window: `[window_start − 7d, window_end)`; event window: `[window_start, window_end)`.
- Assists include the crediting campaign; each campaign once per session; organic never assists; assist-only rows have 0 credited.
- `top_category`: tie → byte-wise smallest; `unknown` competes; null with no purchases.
- Packed-ID order: length in the **lowest** byte (D6).

### Risks still open

| Risk | Status / how we find out |
|---|---|
| Cold-disk read on the scorer could add 5–20 s | **Mostly retired:** V1 ran 40.95 s on the scorer vs 36.2 s locally (1.13×), and V1 reads everything up front. Recheck once V2's total is ~2 s, where I/O could start to show. |
| Practice board dominated by ~0.3 s entries | Almost certainly dataset-specific shortcuts (the generator is public and deterministic; the build step has network). They don't carry over to fresh final data. We don't do this: rules.md says "nothing about a dataset can be hard-coded", and it risks disqualification. Benchmark against our own §9 budget. |
| V1 at 60M: ~19 GB RSS, ~110–130 s | V1 is only the fallback. V2 targets ≤ 8 GB (T15). |
| `parquet` crate build time on the scorer | T01 reports cold build time; fall back to a hand-written writer if > 4 min locally |
| v3 vs v4 | T14 A/B on the board |
| Correctness on unseen seeds | G1 seed sweeps; widen `SEEDS` before every submission |
