# 03 scan-avx2

## Summary
The card's premise needs correcting: `memchr2` is already AVX2, and the scanner-related symbols are 6.96% of sampled work, not the whole 2.8–3.4 s `links+events` phase — the rest is per-row decode and scatter. A fused AVX2 scanner is a 6 h floor (8–10 h with tests) for an estimated 300–500 ms, so it should not be built before the freeze. The shippable part of this track is removing `split::<N>`'s per-line fixed costs (est. 40–100 ms, 1.5–2 h, low risk), which is the half a scanner would later replace.

## Current cost
`00-profile.md` (60M, commit `1f4e467`, one run, partly cold): `links+events` **3,425 ms**, dedup 2,561, sessions 1,319, clicks 572, write 354, internal 8,243 ms / native 8,561 ms. This conflicts with `README.md` §1's 2,811 ms for the same phase; the profile says the warm attempt did not eliminate major faults (266k minor faults in `links+events` vs 113k earlier), so take 2.8–3.4 s as the phase's range, not a code change.

Shares of sampled work (`00-profile.md`, 8,479 samples): `memchr2_raw::find_avx2` 5.88%, `memchr_raw::find_avx2` 1.08% (per-line newline search, `src/v2/scan.rs:36-38`), `scatter::<EvRaw>` task body 9.79%, `scatter::<Ck>` 8.20%, `libc` 6.15%, ts family 5.42%, dedup closure 37.13%.

**Caveat on these numbers (this is the correction the card needs).** The profile has no per-phase CPU/wall split and the sample base is 8,479 samples over 8,479 ms of internal time, so a share is *not* a CPU-second total and must not be multiplied by user CPU or divided by a core count to yield wall time. I use shares only for relative attribution, and bound gains by phase wall and by bytes/rates that are directly measurable.

Directly measurable:
- Bytes: events ≈ 60M × 70 B = 4.2 GB (70 B is `prealloc_cap`'s average, `src/v2/mod.rs:63-66`); clicks ≈ 15M × 45 B = 0.68 GB; ≈4.9 GB of event+click lines, 5.4 GB total input (`README.md` §1).
- End-to-end parse rate: 4.2 GB / 3.425 s / 4 physical cores = **0.31 GB/s per core** (0.37 at the 2.811 s measurement) — matches the card's ~0.35.
- Per line, `split` makes ~8–9 `memchr2` calls (6 commas, ~1.75 quotes from `0.8^7 ≈ 0.21` quote-free lines at `rules.md:132`, plus the terminating miss), each with setup/alignment/tail work (`src/v2/split.rs:29`).

**Answer to the card's GB/s question.** simdcsv's published 0.55007 cycles/byte and 5.29778 GB/s imply ~2.91 GHz effective (5.298e9 / (1/0.55007)), so a 2.3–2.7 GHz Cascade Lake 6230N gets ~4.5–5 GB/s/core at the same cycles/byte — and our dialect needs no PCLMUL quote pass (`rules.md:22`: "every comma separates fields"), so ~6–7 GB/s/core by cycles/byte. Load width, mask iteration and frame overhead derate that to **3–5 GB/s/core** realistic for scan+field extraction; 0.1–0.2 cycles/byte for three compares and `vpmovmskb` would be 13–26 GB/s at 2.6 GHz, which no real line loop reaches. Conclusion: the 0.31 GB/s/core end-to-end rate is not a scanning limit; decode dominates, so a scanner can only remove its own component.

## Ideas

| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | Span-fast `split::<N>`: return `Spans<N>` of `(u32 start, u32 len)` instead of `[Option<&[u8]>; N]` (112 B), fold `field()`'s quote and empty-value tests into the comma loop, drop the `usize::MAX` sentinel | 40–100 (estimate; per-line fixed cost on ~75M event+click lines, unmeasured) | 1.5–2 | low; same dialect traps as the scanner | `src/v2/split.rs`, `parse.rs:14,101`, `people.rs:39` | Existing tests unchanged + 1M-line differential vs `csv::split` (`split.rs:130-190`); G1 ×3; G4 | First half of the scanner card; replaces nothing | **Yes** — best gain/hour here |
| 2 | Fused AVX2 line scanner: per 32 B block emit `,` `"` `\n` masks, carry across blocks, drive line ends and field spans from one bit stream; quote verdict from `popcount==2 && first==start && last==end-1`; **unaligned `loadu` bounded by `line.len()`, scalar tail, no alignment assumption**; `split::<N>` kept as oracle | 300–500 (estimate): scan symbols are 6.96% of sampled work and the scanner should keep ~1/4 of that cost, plus ~20% of the two scatter frames' per-field `Option`/bounds work; bounded above by the 2.8–3.4 s phase and by the serial union-find it overlaps | **6 h floor, 8–10 h realistic** with harness + G1 matrix | med. Empty line ≠ one null field (`split.rs:198`); `""` is null but `"a"` is not (`split.rs:202`); trailing comma keeps count; `\r` is data; never load outside `line.len()` | `src/v2/scan.rs`, `src/v2/split.rs` (or new `blocks.rs`), `parse.rs`, `people.rs`, `mod.rs` | Extend the differential harness (`split.rs:84-190`) to scanner-vs-`csv::split` for N=1,2,4,7; G1 ×3; G4 | Supersedes round-1 `01` #1–#4 and PLAN.md:555; independent of S4-1/S4-2/S4-4 | **No** — 6 h minimum is most of the ≤9 h Pass 2′ budget |
| 3 | Buffered mask flattening on top of 2 (simdcsv's 4-chunk buffering) plus skip quote work when a line's quote mask is 0 (21% of event lines, 41% of click lines) | +80–150 over idea 2 (estimate; simdcsv measured 21% fewer cycles/byte from buffering) | +2–3 | low–med; inherits idea 2's traps | as idea 2 | as idea 2 | Refines idea 2 | No |

## Details
**Why a scanner still wins despite memchr being AVX2.** `split` (`split.rs:28-49`) restarts `memchr2` after every match: ~8–9 calls on a 70-byte event line, each with call setup, a leading alignment step and a tail loop, on haystacks averaging ~8 bytes. A block loop does ~3 unaligned iterations per line and also removes `scan::lines`' separate `memchr` (`scan.rs:36-38`). The quote verdict becomes a mask property instead of a per-field branch (`split.rs:60-77`).

**Loading policy (the part the earlier draft got wrong).** A newline's offset is data-dependent, so no chunking scheme can make record starts 32-byte aligned; copying or padding is out (mmap, 5.4 GB). Therefore: (a) take each line's exact bounds from `scan::lines` and loop `while i + 32 <= line.len()` with `_mm256_loadu_si256`, handling the remaining 0–31 bytes scalar; (b) if a chunk-level loop is preferred, handle the bytes of each record up to the next 32-byte boundary with an explicit scalar prefix and clamp every vector load to `chunk.len()`; (c) never over-read past the chunk, because the last page of a mapping is only zero-filled to the page end, so a 32-B load at EOF can fault. Unaligned loads cost at most ~1 extra cycle per cache-line split (~1 in 16 offsets), which is far cheaper than any alignment machinery.

**Minimal correct version (6 h floor).** One `blocks(line) -> masks` iterator plus a `Spans<N>` cursor; wire only `parse::event` and `parse::click`; leave `people::build` on `split::<2>`; keep `split::<N>` compiled as the oracle and as the `cfg(not(target_feature="avx2"))` path. Accept only if G1 passes in all three configs and the differential harness passes before and after. Budget 6 h as the floor for code, 8–10 h with the harness and G1 matrix.

**Other traps.** `chunks()` guarantees whole lines (`scan.rs:42-67`), so no quote/line state crosses chunks — do not weaken that. A zero-length line must stay `Skip`, not become one empty field. `id::pack` overflow must still surface as `Overflow` (`parse.rs:20,57`) so the V1 fallback still fires.

## Rejected ideas
- **64-B-aligned chunk starts** (previous draft's idea 3): invalid. Moving a boundary to the next `\n` cannot guarantee alignment; replaced by the unaligned-load + bounded-tail + scalar-prefix policy above.
- **SWAR (u64) stepping stone** (round-1 `01` #2): ~0.5–0.75 ops/byte vs ~0.1 for AVX2, so it lands at or below today's `memchr2`; 8–12 h for little.
- **Fold the newline search into `split` with `memchr3`**: removes the 1.08% `memchr` share but adds a third compare to every match loop — expected wash.
- **Chunk-level newline index from one `memchr_iter`**: saves only per-call setup, unproven, ≤50 ms.
- **RFC 4180 quote state / PCLMUL prefix-XOR**: wrong dialect (`rules.md:22`).
- **AVX-512**: out per `x86-64-v3` (`.cargo/config.toml:2`); T14 dropped.
- **Store all masks, then parse in a second pass** without buffering: extra traffic; folded into idea 3.
- **SIMD timestamp/price/type decode**: track `02`; mixing it in makes a 10 h card.

## Dependencies / conflicts
S4-1/S4-2 (`mod.rs` P2/P3 handoff, bucket bits) do not touch the line loop → compatible. If `dedup-arch` picks a two-pass "keys then winners" parse, the scanner must expose key spans separately — settle after synthesis. `02` (ts SWAR) also edits `parse.rs`: share the `Spans` type or land `02` first. Idea 1 touches every call site, so land it before any other `parse.rs` card. Nothing here needs a new dependency (`memchr`, AVX2 via `x86-64-v3`).

## Sources
- `research/perf2/00-profile.md` (phase table, fault counts, sample shares); `research/perf2/README.md` §1; `research/perf/SYNTHESIS.md` §1–2, §6; `research/perf/01-simd-csv.md` (round-1 estimate, now stale); `research/perf/02-field-parsing.md`.
- `src/v2/split.rs:29,60-77,84-190`; `src/v2/scan.rs:36-38,42-67`; `src/v2/parse.rs:14,101`; `src/v2/people.rs:37-41`; `src/v2/mod.rs:63-66`; `src/id.rs:10-20`; `.cargo/config.toml:2`.
- `rules.md:22` (dialect, every comma separates fields), `:50-53` (dedup before validation), `:123-133` (ts mix, quoted/malformed shares).
- `PLAN.md:160-178` (O1–O3, O8), `:346-349` (D3 memchr, T14, T15), `:491-555` (Pass 2 cards; scanner out of these passes), `:568-571` (G1/G3/G4).
- https://github.com/geofflangdale/simdcsv (pipeline, RFC 4180 quote pass); https://deepwiki.com/geofflangdale/simdcsv/7.3-performance-metrics (0.55007 cycles/byte, 5.29778 GB/s; 0.694/4.27 before buffering); https://branchfree.org/2019/03/06/code-fragment-finding-quote-pairs-with-carry-less-multiply-pclmulqdq/ (quote-pair bitmasks, not needed here); simdjson stage-1 structural indexing, arXiv:1902.08318.

## Recommended card

**S5-1 — Span-fast `split`** · Scope: `src/v2/split.rs`, call sites `src/v2/parse.rs:14,101`, `src/v2/people.rs:39` · Depends: none · 1.5–2 h · est. 40–100 ms at 60M

- Replace `[Option<&[u8]>; N]` with `Spans<N>` = `[(u32, u32); N]` byte offsets into the line, plus an explicit `wrong field count` result. Fold `field()`'s quote test (`quotes == 2 && first == start && end > start + 1 && line[end-1] == b'"'`) and its empty-value test into the comma loop, dropping the `usize::MAX` sentinel. Callers slice once and convert to `Option` per field.
- No alignment change, no chunk-boundary change, no intrinsics: this card is pure refactor of the existing `memchr2` loop.
- **Accept:** G1 in all three configs (default; `SHOPT_CHUNK_BYTES=4096`; `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`), G4 `MATCH` on p20, `cargo test split scan parse`, and `make bench DATA=data/p20` median no worse than +0% (revert if flat — the estimate may be optimistic, since LLVM may already keep the array in registers).
- **Traps:** empty line stays `Skip`; `""` stays a null field but `"a"` does not; trailing comma keeps the field count; `\r` is data; `id::pack` overflow still returns `Overflow`.
- **Do not** start the AVX2 scanner in this card. It is a 6 h floor (8–10 h realistic) decision for the synthesis, and only worth taking if the dedup work is dropped or shrinks enough to leave a clean 6-hour block before Sun 09:00.
