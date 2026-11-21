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
5. **Sandbox:** agents cannot reach GitHub (no push, pull or fetch) and cannot use `sudo`. Hand those to the human.
6. Do not relitigate §4 decisions. If a task seems to need a different decision, stop and ask the orchestrator.
7. The pipeline writes logs to **stderr only**. Stdout stays effectively empty (the scorer caps it at 50 MB).
8. Never commit `data/`, `.tools/`, `.venv/`, `target/`, `out*/`, or generated datasets. The repo must stay < 50 MB.
9. Dependencies are limited to the allow-list in §4. Anything else needs orchestrator approval (risk: the 10-minute scorer build).

---

## 1. Goal and scoring

Read messy CSVs from `/data`, write two Parquet tables to `/output/user_daily/` and `/output/campaign_daily/`, exactly as `rules.md` defines.

- **Score = wall-clock seconds, container start → exit. Lower wins.** Any wrong row scores nothing.
- Run box: linux/amd64 GCP n2 Xeon (Cascade Lake or newer), **8 vCPU, 24 GB RAM, no swap, no network**. `/data` read-only, `/scratch` writable, root FS read-only (`/tmp` 64 MB), uid 1000. Env: `DATA_DIR OUTPUT_DIR SCRATCH_DIR NCPU=8 MEM_GB=24`.
- Gate: public dev set (1M events) must match, < 3 min. Practice board: hidden ~20M events. **Final: fresh ~60M events, 3-min cap per run, median of 3**; the top 5+ are re-timed 5×.
- Hidden-data feedback is only exit code, OOM flag, and counts of missing/extra/differing rows.
- Limits: **3 submissions/hour** (was 2; can change), a rejected entry still counts, ties go to the earlier submission, ~20% machine variance. See §8 for the form.
- Board at time of writing: baseline (PySpark) 171.91 s, leader 16.38 s (20M practice).
- Image build on the scorer has network, **≤ 10 minutes**.

---

## 2. Current state

| Path | What | Status |
|---|---|---|
| `rules.md` | The spec | given, frozen |
| `fixtures/` | 36 edge cases: `input/` + `expected/*.csv` | given, frozen |
| `reference/reference.py` | Literal Python implementation. 36/36 fixtures, matches `dev-answer`. ~13 s per 1M events | done, frozen |
| `scripts/run_fixtures.py` | Runs `CMD` on every fixture, exact diff | done |
| `scripts/compare.py` | Exact order-insensitive diff of two output folders | done |
| `scripts/diff_seeds.py` | Runs candidate + reference on many generator seeds, diffs (cached in `data/`) | done |
| `scripts/docker_run.sh` | Runs the image with the scorer's exact flags | done |
| `scripts/build_generator.sh` | Builds `generator.zip` → `.tools/generate` | done |
| `Makefile` | `setup dev fixtures check-dev diff-seeds build docker-fixtures run-dev` | done |
| `dev-answer.zip` | Expected output for seed 1, 1M events (117,038 user rows, 27,400 campaign rows) | given |
| `Cargo.toml`, `src/`, `Dockerfile` | The Rust pipeline | **not started** |

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
| D3 | Dependency allow-list: `parquet` (`default-features = false`, no arrow, uncompressed), `rayon`, `memmap2`, `mimalloc`, `hashbrown`. Nothing else without approval. | Scorer build ≤ 10 min. |
| D4 | Docker: builder `rust:1.97-bookworm` (matches local rustc 1.97) with a dependency-cache layer; runtime `gcr.io/distroless/cc-debian12`; `ENTRYPOINT ["/shoptimizer"]`. | Small image, fast start. |
| D5 | Build flags in `.cargo/config.toml`: `target-cpu=x86-64-v3`, plus release profile `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`. `x86-64-v4` only via the T13 A/B. | AVX-512 downclocks Cascade Lake. Local Zen 4 has no penalty, so local tests flatter v4. |
| D6 | **Packed IDs (V2):** `u128`, ID bytes big-endian from bit 127 down, zero-padded, **length in the lowest byte**. Integer `<` then equals byte-wise order. IDs > 15 bytes: the whole run falls back to the V1 path (a flag set during parse). | No shared interning table. Measured: all IDs ≤ 9 bytes in generated data, so the fallback is for safety only. |
| D7 | **No click dedup.** Copies are identical, so they can't change credit or assists. Assist window counts only need `> 0`. | Saves a hash pass over ~15M clicks. |
| D8 | **Dedup events globally first** (partition by `event_id` hash), **then** re-partition by person. | The kept copy may name a different user (E4). |
| D9 | Parallel parse splits files into **fixed byte ranges aligned to `\n`**, size from env `SHOPT_CHUNK_BYTES` (default 32 MiB). | File sizes are skewed (one file ≈ 16% of the data). The env var lets small tests exercise many chunks. |
| D10 | Parquet: `INT64` for counts, `INT32` + logical `DATE` for `day`, `BYTE_ARRAY` + `String` for strings, `top_category` nullable, no compression. | Matches the checker. Cheap to write. |

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
// src/v2/…  — parallel pipeline (T7–T11), same output; falls back to v1::run when D6's flag trips.
```

Data flow (V2): `mmap files → byte-range chunks → parse (parallel) → scatter by hash(event_id) → dedup + validate + late (per bucket) → map user → person, scatter by hash(person) → sort (person, ts) → sessionize + credit/assists against that person's clicks sorted (ts, click_id) → per-thread aggregates → merge → write Parquet`.

---

## 6. Tasks

### Dependency graph

```
H1 ─┬─ T00 ─┬─ T01 ─┬─ T02 ─┐
    │       │       ├─ T03 ─┼─ T05 ── H2 (submit V1)
    │       │       ├─ T04 ─┤
    │       │       └─ T06 ─┘
    │       └─ T07 (bench tooling, any time after T00)
    └─ (repo public before H2)
T05 ─┬─ T08 (parallel parse) ─┐
     ├─ T09 (packed IDs) ─────┼─ T10 (parallel dedup + repartition) ── T11 (parallel sessions/agg) ── H3 (submit V2)
     └─ T12 (parallel Parquet, any time)
H3 ── T13 (profile + tune loop) ── T14 (flag A/B) ── T15 (60M robustness) ── H4 (final pick, freeze)
```

Tasks on the same row with no arrow between them can run **in parallel** (T02/T03/T04/T06; T08/T09/T12).

### Task cards

Each card: **Scope** (files the task may touch), **Depends**, **Do**, **Accept** (all must pass), **Traps**.

---

**H1 — Human setup** · Depends: — 
- `sudo pacman -S docker-buildx`. Confirm `docker buildx version`.
- Make the GitHub repo **public** before the first submission (Settings → Danger Zone). Fill in `TEAM.md` (team name, members, engine: Rust).
- Accept: `docker buildx version` prints a version; the repo URL opens logged out.

---

**T00 — Baseline environment** · Scope: none (no commits) · Depends: — 
- Do: `make setup && make dev`; generate `data/p20` (§3). Run `make fixtures`, `make check-dev`, and `make diff-seeds SEEDS=2-3` with the default `CAND` (the reference).
- Accept: fixtures 36/36 pass; `check-dev` prints `MATCH`; diff-seeds runs (reference vs itself). Report wall times.
- Note: `data/p60` is only needed from T15 on (5.5 GB of disk).

---

**T01 — Crate skeleton, Dockerfile, empty outputs** · Scope: `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `src/**` (stubs only), `Dockerfile`, `.dockerignore`, `Makefile` (only to add `CAND` default for Rust if wanted) · Depends: T00, H1 (for docker)
- Do: create the crate with every module and signature from §5 as `todo!()` stubs, **except** `output.rs` and `main.rs`, which must work: write both tables with **zero rows** and the exact schema (D10). Dockerfile per D4 with a dependency-cache layer. Add `data/ .tools/ .venv/ target/ out*/ reference/ scripts/` to `.dockerignore` (keeps the build context small).
- Accept:
  - `cargo build --release` succeeds; `cargo test` succeeds.
  - `make fixtures CAND=target/release/shoptimizer`: cases whose expected tables are empty (E5, E13) pass; every other case fails only on missing rows, not on schema.
  - `make build && make docker-fixtures`: same result through the image.
  - Cold `docker build --no-cache` takes < 4 min locally. Report the time and image size.
- Traps: `day` must be a Parquet DATE (`INT32` + Date logical type), not a string or timestamp. Exact column names, no extras.

---

**T02 — CSV dialect** · Scope: `src/csv.rs` · Depends: T01
- Do: implement `data_lines` and `split` exactly per rules.md "CSV dialect". Unit tests in the same file.
- Accept: `cargo test csv` passes, with tests for at least: quoted field `"u1"` → `u1`; `""` → null; empty → null; wrong field count → None; empty line → None; `u1"` / `"u1` / `u"1"` / `""x""` → None; ` u1` keeps its space; trailing `\r` stays in the last field; header skipped; final line without `\n` is a line; header-only file yields nothing; a later line equal to the header is data.
- Traps: never trim, never case-fold. A quote is only valid as the first and last byte of a field, with no quote in between.

---

**T03 — Timestamps** · Scope: `src/ts.rs` · Depends: T01
- Do: implement `parse_ts`, `day_of`, and manifest-window parsing per rules.md "Timestamps". Unit tests in the same file.
- Accept: `cargo test ts` passes, with tests for at least: the F1/F2/F3 examples from rules.md giving the same instant; F3 with leading zeros; F3 `86400000` → Some (out of window is decided elsewhere); a 21-digit F3 → None or out-of-window, **no panic**; `1791071890000.0`, `1.79e12`, `-5` → None; non-ASCII digits (`٢`, `２`) → None; fractional seconds, lowercase `z`/`t`, space for `T`, no zone → None; Feb 29 valid in 2028, invalid in 2027; month 13, Feb 30, hour 24, second 60 → None; offsets `+23:59`, `-00:00` valid; `+24:00`, `+05:60`, `+0200` → None; an offset that changes the date (E7); `day_of` of negative instants floors correctly.
- Traps: subtract the offset to get UTC. F3 "too long for i64" must not wrap.

---

**T04 — Linked IDs** · Scope: `src/people.rs` · Depends: T01
- Do: union-find over byte-string IDs with path compression; name = byte-wise min of the component. Unit tests.
- Accept: `cargo test people` passes, with tests for at least: chains in either direction; a 400-link chain (no recursion overflow); self-link no-op; repeated link either way round; an ID only seen in links can be the name; `U7` sorts before `u…`; unlinked ID names itself.
- Traps: iterative `find` (chains are hundreds long, shared devices merge thousands of IDs).

---

**T05 — V1 pipeline (single-threaded, correct)** · Scope: `src/v1.rs`, `src/main.rs` · Depends: T02, T03, T04, T06 (or T06 can be folded in)
- Do: implement rules.md "Events, in this order", "Clicks", and "Outputs" literally, using the modules above. Mirror `reference/reference.py`'s structure where unsure.
- Accept (gate **G1** + **G2**, §7). Also report the native time on `data/dev` and `data/p20`.
- Traps: see §9 "Trap list". Highest-risk items: dedup before validation (E5); the kept copy's user (E4); equal timestamps around a gap (E16); credit tie-break = smallest `click_id` (A5); assist-only campaign rows (A11).

---

**T06 — Output aggregation helpers** · Scope: `src/output.rs`, `src/agg.rs` (new) · Depends: T01
- Do: per-session accumulation → `UserDaily` rows (incl. `top_category`: max summed purchase cents, tie → byte-wise smallest, `unknown` competes, null if no purchases) and `CampaignDaily` rows (credited columns + `assisted_sessions`, organic has 0 assists, assist-only rows have 0 credited). Unit tests.
- Accept: `cargo test agg` passes, with tests for the E8, E9, A11 and organic cases.

---

**H2 — Submit V1** · Depends: T05 merged, H1
- Human: push `main`, submit the SHA on the track page, fill a row in §8.
- Accept: dev check passes on the site; practice time recorded.

---

**T07 — Bench tooling** · Scope: `scripts/bench.sh` (new), `Makefile` (add `bench` target) · Depends: T00
- Do: `scripts/bench.sh DATA [RUNS=3]` runs the Docker image via `scripts/docker_run.sh` on `DATA` with `CPUS=8 MEM=24g`, prints each wall time and the median, and checks the output against a cached reference output for that dataset (generate it once with the reference; ~4 min at 20M).
- Accept: `make bench DATA=data/p20` prints 3 times + median and `MATCH`.

---

**T08 — Parallel chunked parse** · Scope: `src/v2/read.rs`, `src/v2/mod.rs` · Depends: T05
- Do: mmap every CSV; split into byte ranges per D9 (each range starts after a `\n` and ends at one; the first range of each file skips the header); parse in parallel with rayon into per-thread `Vec`s of compact records. Use the V1 parsing functions.
- Accept: G1 run **with `SHOPT_CHUNK_BYTES=4096`** (forces many chunks per file) and with the default; G3.
- Traps: the header belongs only to byte 0 of each file. A range boundary landing exactly on `\n`. Empty and header-only files.

---

**T09 — Packed IDs** · Scope: `src/id.rs` (new) · Depends: T05
- Do: `PackedId(u128)` per D6: `pack(&[u8]) -> Option<PackedId>` (None if > 15 bytes), `unpack`, `Ord` = integer order. A global "fallback needed" flag set by any `None`.
- Accept: `cargo test id` passes, with a property test that, for random byte strings ≤ 15 bytes (incl. `0x00`, spaces, upper case, prefixes), `pack(a) < pack(b)` ⇔ `a < b` byte-wise, and that `pack` is injective. Explicitly: `U1 < u1 < "u1 " < u10 < u2`.

---

**T10 — Parallel dedup + re-partition by person** · Scope: `src/v2/dedup.rs`, `src/v2/people.rs` · Depends: T08, T09
- Do: scatter parsed events into 2^k buckets by hash(event_id); per bucket keep the smallest `ingest_ts` copy (malformed lines are already gone), then validate and apply the late rule. Map each surviving event's user → person name (People built on packed IDs), scatter by hash(person). Clicks: validate, map to person, scatter by the same hash. No click dedup (D7).
- Accept: G1 (both chunk sizes); G3.
- Traps: validation happens **after** choosing the kept copy. Equal `ingest_ts` copies are identical (either is fine).

---

**T11 — Parallel sessions, attribution, aggregation** · Scope: `src/v2/sessions.rs`, `src/v2/mod.rs` · Depends: T10
- Do: per person-bucket in parallel: sort events by (person, ts), sort clicks by (person, ts, click_id); sessionize; for each session start, credit = the **first** click of the latest-`ts` group with `ts ≤ start` and `start − ts ≤ 604,800,000`; assists = campaigns with count > 0 in the sliding window `[start − 7d, start]`. Accumulate into per-thread aggregates, merge, write.
- Accept: G1 (both chunk sizes) + G2; G3. Report p20 bench.
- Traps: the window includes both ends. Equal-`ts` events join the session of the first of them. One click credits many sessions.

---

**T12 — Parallel Parquet write** · Scope: `src/output.rs` · Depends: T05
- Do: split `user_daily` rows into N row groups/files written in parallel (`part-0..N.parquet`).
- Accept: G1; G3 (output phase time reported before/after).

---

**H3 — Submit V2** · Human, same protocol as H2.

---

**T13 — Profile and tune loop** · Scope: `src/**` · Depends: H3
- Do: `perf record` the binary on `data/p20` natively; attack the top phase. One idea per branch, each measured with `make bench`. Candidate ideas: SIMD/`memchr` field scanning; parse only needed fields; per-thread pre-sized hash tables; radix sort on (person, ts); reuse buffers (zero per-row allocation); `mimalloc`; overlap I/O and parse; drop out-of-window/bad-type rows right after dedup.
- Accept per idea: G1 + G3 (≥ 3% median win on p20 or reverted).

---

**T14 — Build-flag A/B** · Scope: `.cargo/config.toml`, `Cargo.toml` · Depends: H3
- Do: produce two commits, `x86-64-v3` vs `x86-64-v4` (and mimalloc on/off if not already settled). The human submits both and keeps the faster (board variance ~20%: only trust gaps larger than that, or repeat).
- Accept: both pass G1 + G2.

---

**T15 — 60M robustness** · Scope: none unless a fix is needed · Depends: T11
- Do: `make bench DATA=data/p60` (generate the reference output once). Measure peak RSS (`/usr/bin/time -v` natively, or `docker stats`).
- Accept: `MATCH`; median < 30 s locally; **peak RSS < 12 GB**; no writes outside `/output` and `/scratch`.

---

**H4 — Final pick and freeze** · Human · Sun 09:00 AM ET
- Pick the fastest commit that passed G1 + G2 + an accepted practice run. Make sure it is pushed and the repo is public. No new submissions after freeze unless the current best is broken.

---

## 7. Gates

| Gate | Commands (with `C=target/release/shoptimizer`) | Pass |
|---|---|---|
| **G1 correctness** | `cargo test` · `make fixtures CAND=$C` · `make check-dev CAND=$C` · `make diff-seeds CAND=$C SEEDS=2-30` · `.venv/bin/python scripts/diff_seeds.py --seeds 41-43 --events 2000000 --clicks 500000 -- $C` | all tests pass, 36/36, `MATCH`, every seed matches |
| **G2 docker** | `make docker-fixtures` · `make run-dev` | 36/36, `MATCH`, dev run < 30 s |
| **G3 performance** | `make bench DATA=data/p20` vs the best recorded in §8 | median no worse than +5% (unless the change is a correctness fix) |

V2 tasks run G1 twice: default chunk size and `SHOPT_CHUNK_BYTES=4096`.

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
| `user_daily` rows | 117,038 | | ~7M |

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
| Cold-disk read on the scorer could add 5–20 s | Compare the first practice time to local p20; per-phase stderr logs on the dev check |
| `parquet` crate build time on the scorer | T01 reports cold build time; fall back to a hand-written writer if > 4 min locally |
| v3 vs v4 | T14 A/B on the board |
| Correctness on unseen seeds | G1 seed sweeps; widen `SEEDS` before every submission |
