# 08 io-memory

> **Orchestrator validation note (supersedes conflicting claims below):** Cold throughput, fault behavior and file-cache charging depend on the actual storage and kernel. A fresh container does not prove cold data. Read process peak RSS from `/proc/self/status` (VmHWM), not smaps_rollup; monitor cgroup peak separately.

## Summary
With mmap already in place, this track's warm-cache headroom is small: I can bound every plausible idea (mimalloc purge policy, windowed read-ahead, parallel prefault) at roughly 0–250 ms of a projected ~8,900 ms at 60M, and the ideas are partly non-additive. The real io-memory risk is not the mechanism but the cold-disk floor and the RSS measurement: 5.5 GB mapped input makes the T15 "peak RSS ≤ 8 GB" gate unmeasurable as written, and a cold Persistent Disk can cost 4.6–40 s regardless of mmap vs `read`.

## Current cost
Directly observed (p20, `NCPU=8 taskset -c 0-3,8-11`): open 8–11 ms; links+events 1,082–1,246 ms for ~1.8 GB of input; peak RSS 5.5 GB including 1.8 GB mapped (`research/perf/00-profile.md`, table and §"Symbol sample"; `PLAN.md:202`). `open` is under 0.4 % of native total (`research/perf/INDEX.md`), so restructuring mapping costs nothing on its own.

Projected at 60M (reasoning, not measured): files ~5.5 GB, i.e. ~1.44 M 4 KiB pages; phases scale ~3× from the p20 table (§1 packet) to ~8.9 s locally, ~10 s on the scorer at the measured 1.13× CPU factor.

Two bounds frame everything below:
- **Warm page cache.** Every input byte is touched exactly once (`scan::chunks` → `scan::lines`, `src/v2/scan.rs:42,69`), so page faults are paid once and amortized. The kernel fault-around path maps ~16 pages (64 KiB) per fault, so 5.5 GB costs ~90 K faults rather than 1.44 M; at a hypothesised 0.5–2 µs per minor fault that is 0.05–0.18 s of CPU, ~0.01–0.05 s wall on 4 physical cores. The kernel's own THP rationale says the "one fault per 2 MB" effect is "almost completely irrelevant" (`transhuge.rst`), which is the correct prior here.
- **Cold disk.** `/data` is a GCP Persistent Disk. Zonal PD read throughput is baseline 140 MiB/s (balanced) / 240 MiB/s (ssd) plus 0.28 / 0.48 MiB/s per GiB, capped at 1,200 MiB/s per instance (`cloud.google.com/compute/docs/disks/performance`). 5.5 GB therefore takes ≥ 4.6 s cold and plausibly 20–40 s on a small balanced disk — dwarfing all 12 pipeline tracks. Inference from V1: 40.95 s on the scorer vs 36.2 s local is exactly the 1.13× CPU factor, leaving ~0 s residual for I/O (`PLAN.md:441`), consistent with a warm page cache in that run — one observation, not proof.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | mimalloc purge policy: `purge_delay = -1` (no `MADV_DONTNEED`), set programmatically via `mi_option_set` | 30–150. Default v2.1.7 `purge_delay` is 10 ms and `purge_decommits=1` (`options.c`), so pages freed and reused later are decommitted then re-faulted; profile shows `mi_page_free_list_extend` 3.21 % plus malloc entry points ~0.6 % (`00-profile.md`) | 2 | none (allocation only) | `src/main.rs:15`, `Cargo.toml` (needs `libmimalloc-sys` — allow-list check), `Dockerfile` | G1, G4; `MIMALLOC_SHOW_STATS=1` delta; `memory.stat` `pgfault`/`anon` |
| 2 | Replace the blanket `Advice::WillNeed` with per-chunk windowed read-ahead + `MADV_SEQUENTIAL`, and `MADV_DONTNEED` consumed spans | 0–40 warm (open is only 8–11 ms today, so there is almost nothing to win); cold: avoids the pathological alternative of a synchronous 5.5 GB prefault, saving up to 2,000–4,000 versus MAP_POPULATE-style serialisation | 3 | low — mappings are read-only and immutable; must not free a span a live chunk slice still borrows (`scan.rs:26-27` SAFETY) | `src/v2/scan.rs:12-31`, `src/v2/mod.rs:62-68` | G1 default + `SHOPT_CHUNK_BYTES=4096`, G4; fault counters per phase |
| 3 | Parallel prefault one morsel ahead (`MADV_POPULATE_READ` on the next chunk from a dedicated thread) | 0–150. Only pays if instrumentation shows >0.3 s of fault/kernel time; total fault CPU is bounded at ~0.05–0.18 s warm (above), so most of this range is insurance for a cold disk | 5 | med — overlaps I/O with parse, changes peak RSS and I/O interleaving | `src/v2/scan.rs`, `src/v2/mod.rs:76-99` | G1, G4, plus cold-cache A/B (`echo 3 > drop_caches` requires root; use a fresh container) |
| 4 | Instrument the gap: per-phase `minflt`/`majflt` from `/proc/self/stat`, `RssAnon`/`RssFile`/`VmHWM` from `smaps_rollup`, behind `SHOPT_IO_STATS=1` | 0 direct; gates ideas 1–3 and settles the 8.69 % "unresolved libc" group (`00-profile.md`), which is *not* attributable without this | 2 | none | `src/v2/mod.rs:31`, `src/main.rs:50` | G1 (stderr-only logging, `PLAN.md:26`) |

Non-additivity: ideas 1–3 all attack different slices of the same ≤ ~350 ms of memory-management CPU, and idea 2's warm gain overlaps idea 3's. Amdahl: even 250 ms against 8,900 ms is 2.8 %, below G3's 3 % bar unless combined with another track.

## Details

### 1 mimalloc purge policy
`mimalloc = "0.1"` → `libmimalloc-sys 0.1.49` / mimalloc v2.1.7 (`Cargo.lock:278-303`). In `src/options.c` v2.1.7 the defaults are `purge_delay = 10` ms and `purge_decommits = 1`, i.e. unused pages get `MADV_DONTNEED` (immediate RSS drop, immediate re-fault on reuse). A 3 s run with Vec growth, `collect` into `Vec<Vec<Vec<_>>>` and per-bucket hash tables churns hard enough that pages purged at t and reused at t+50 ms pay fault + zeroing twice. Setting `purge_delay = -1` keeps pages committed; cost is RSS, ~0.3–1.5 GB at 60M, which collides with the T15 gate below.

Caveat that decides implementation: mimalloc v3 documentation states "Elevated processes ignore all environment options", and the distroless image runs as root (`Dockerfile`, no `USER`). Whether v2.1.7 applies the same check was not verified; therefore set the option through `mi_option_set(mi_option_purge_delay, -1)` before the first allocation rather than `std::env::set_var`, and note `libmimalloc-sys` as an additional (transitive) dependency. `MIMALLOC_ALLOW_LARGE_OS_PAGES` is not viable: it needs a hugetlbfs pool (`mimalloc environment docs`).

### 2 Windowed read-ahead
Today `map_dir` maps every file and issues one whole-file `MADV_WILLNEED` (`src/v2/scan.rs:28-29`), matching O11 (`PLAN.md:180`) and the T21 contract (`PLAN.md:279`). The advantage already exists: because faults are lazy, the first chunk's pages are faulted while other workers parse, so mmap overlaps I/O with compute for free. Two refinements: (a) mark event/click/link maps `MADV_SEQUENTIAL` so the kernel keeps read-ahead aggressive and may drop pages behind the scan; (b) issue `MADV_WILLNEED` on a bounded 32 MiB window ahead of the chunk currently being taken, so a 5.5 GB advice call never becomes a long synchronous submit in the open phase. Because every byte is touched once and no chunk is revisited, drop-behind is semantics-free. Restructuring must keep chunk boundaries byte-identical (header exclusion, final line without `\n`) — the p20 traps at `PLAN.md:425-438` are all chunking-visible — hence `SHOPT_CHUNK_BYTES=4096` in G1.

### 3 Parallel prefault
`MADV_POPULATE_READ` (memmap2 `Advice::PopulateRead`, `docs.rs/memmap2/0.9.9`) fills page tables without a handler. Value is zero if parse threads already saturate the cores: moving a fault from a parser to a helper thread does not reduce total CPU, only the tail where a faulting worker stalls. On a cold disk it converts random-ish per-worker stalls into a single sequential stream — a genuine but unquantified win. Keep behind an env switch and only adopt if idea 4 shows majflt > 0.5 s.

### 4 Instrumentation
Log `minflt`/`majflt` deltas and `smaps_rollup` at each `phase()` (`src/v2/mod.rs:31,68,100,125,167,194,200`). This is the only way to separate "fault cost", "libc memcpy", and "I/O wait" inside the 8.69 % unresolved-libc bucket; note that a `SIGPROF`-based profile attributes a fault stall to the faulting *user* instruction, so the 23.72 % in `split::<7>` may already contain fault time, and I/O wait contributes no samples at all.

## Rejected ideas
- **`read()` into buffers instead of mmap.** Adds a 5.5 GB copy (~0.5–0.7 s CPU, ~0.15 s wall on 4 cores) and doubles buffer footprint; V1 already reads (`src/v1.rs:178-184`) and shows no advantage.
- **`MAP_POPULATE` on the whole 5.5 GB.** Serialises all I/O and page-table work into `open`; on a cold disk that is the 4.6–40 s floor paid up front, and on a warm cache it converts parallel demand faults into one thread's work.
- **THP for the CSV mappings.** Kernel: "Currently THP only works for anonymous memory mappings and tmpfs/shmem" (`transhuge.rst`); `MADV_HUGEPAGE` is documented as private-anonymous only (`memmap2` Advice docs). No file-backed THP for /data short of `CONFIG_READ_ONLY_THP_FOR_FS` executables.
- **`mlock` / `MAP_LOCKED`.** Needs `RLIMIT_MEMLOCK`, buys nothing over page cache here.
- **`O_DIRECT`.** No page cache reuse across the click/link/event passes; would add alignment work and defeat fault-around.
- **Dropping maps early to cut RSS.** No wall gain; note it as an RSS-gate lever only.

## Dependencies / conflicts
- Peak RSS model conflicts with T15's ≤ 8 GB: 1.8 GB mapped at 20M ⇒ 5.5 GB mapped at 60M, plus ~3× the ~3.7 GB anon working set ⇒ ~16 GB RSS worst case, 69 % of 24 GB. `RssFile`/`file_mapped` is reclaimable page cache charged to the container cgroup (`memory.stat` `file`, `file_mapped`, `pagemax` semantics in `cgroup-v2`), so the gate should be read as `RssAnon`, not `VmHWM`. This needs an orchestrator decision.
- Idea 1 conflicts with any track that grows the working set (partition-scatter, dedup): purge-off raises anon RSS further.
- Idea 2 touches `scan.rs`/`mod.rs`, shared with parse-work-avoidance and pipeline-shape.
- The 8.69 % libc bucket is shared with partition-scatter (memcpy on Vec growth) and dedup (hashbrown); do not double-count it.

## Sources
- `research/perf/00-profile.md` (phase table, symbol sample), `research/perf/INDEX.md` (open < 0.4 %).
- `PLAN.md:180` (O11), `:202` (peak RSS 5.5 GB), `:279` (T21 contract, 32 MiB chunks, WILLNEED), `:336` (T13 MAP_POPULATE candidate), `:337` (G3), `:345-346` (T15, ≤ 8 GB), `:357-360` (G1/G4), `:425-438` (trap list), `:441` (cold-disk risk).
- `src/v2/scan.rs:12,26-29,42,69`; `src/v2/mod.rs:62-68,76-99`; `src/main.rs:15-16,66-73`; `src/v1.rs:178-184`; `Cargo.lock:278-303`.
- mimalloc v2.1.7 `src/options.c` (defaults `purge_delay = 10`, `purge_decommits = 1`): https://raw.githubusercontent.com/microsoft/mimalloc/v2.1.7/src/options.c
- mimalloc environment options (v3 "elevated processes ignore all environment options"): https://microsoft.github.io/mimalloc/environment.html
- GCP Persistent Disk throughput/per-GiB and 1,200 MiB/s instance cap: https://cloud.google.com/compute/docs/disks/performance
- Transparent Huge Page support (anon/tmpfs only; fault-count effect "almost completely irrelevant"): https://www.kernel.org/doc/html/latest/admin-guide/mm/transhuge.html
- `madvise(2)` (`MADV_WILLNEED`, `MADV_SEQUENTIAL`, `MADV_DONTNEED` repopulates from file): https://man7.org/linux/man-pages/man2/madvise.2.html
- memmap2 `Advice` (`PopulateRead`, `HugePage` anon-only note): https://docs.rs/memmap2/0.9.9/memmap2/enum.Advice.html
- cgroup v2 memory controller (`memory.current`, `memory.peak`, `file`, `file_mapped`, `pgfault`, `pgmajfault`): https://docs.kernel.org/admin-guide/cgroup-v2.html#memory
- Fault-around (~16 pages per fault) is an unverified-from-primary-source assumption here; treat the 0.05–0.18 s fault-CPU bound as a hypothesis pending idea 4.
