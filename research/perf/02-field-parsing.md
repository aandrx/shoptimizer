# 02 field-parsing

> **Orchestrator validation note:** Treat the bit tricks as unverified sketches, not code: compare the sign explicitly to `+`/`-`, never read a fourth byte from a three-byte tail, and differential-test all byte classes. A day-table miss must fall back to the generic parser, particularly for ingest timestamps that participate in dedup before validation. The UTC conversion subtracts the offset. Absence from the flat top-40 cannot bound an inlined helper’s cost; the gain figures are hypotheses, not measured ceilings.

## Summary
The best idea is to make the timestamp shape dispatch do the format work: validate F1/F2 separators and digits with one SWAR mask and resolve the calendar through a window-bounded day table, keeping the existing checked arbitrary-length F3 loop as the slow path. Estimated ~150–220 ms at 60M (~2% of the ~8.9 s projected run), mostly from the ~4.9% of user CPU now in `parse_civil`/`days_from_civil`/`parse_ts`. Main risk is a fast path that silently rejects legal leading-zero F3 or out-of-window F2 dates, so the checked loop must stay reachable and tested.

## Current cost
`rules.md` fixes the form mix: F1 ~58%, F2 ~10%, F3 ~32% of timestamps, 0.3–0.7% bad (`rules.md:123-124`). `parse_ts` (`src/ts.rs:15`) tests *all* digits first, so F1/F2 pay a short-circuited scan that fails at `s[4]`, then a length/zone check, then `parse_civil` (`ts.rs:172`) with six digit fields, leap/`days_in_month` checks and `days_from_civil` (`ts.rs:249`, three divisions plus multiplies). Event rows call it twice (`ts`, `ingest`; `src/v2/parse.rs:13,28`), clicks once (`parse.rs:95`).

Profile (user-CPU samples, not wall): `parse_civil` 1.79% + `days_from_civil` 1.72% + `parse_ts` 1.41% = **4.92%** of sampled CPU; `csv::split::<7>` 23.72%; the 22.21% `<run_v2::{closure#5}>` bucket is **dedup**, not the row parsers (`research/perf/00-profile.md`). Inlined `parse_fixed_digits`/`parse_digits` are charged to their callers, so 4.92% is a floor. At 60M the naive 3× projection is ~8.9 s locally (**estimate, not measured**); 5% of that is ~450 ms of CPU, i.e. roughly 150–220 ms of wall depending on how much of it stays on the critical parse phase. `parse_price` (`src/v1.rs:98`) and `kind_of` (`v1.rs:134`) do not appear in the top-40, so each is <0.5% of CPU.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk (none/low/med/high + which trap) | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Dispatch on length+zone bytes first; validate F1/F2 digits and `-`/`:`/`T` with one SWAR mask per 8-byte word inside the already-sliced 19-byte civil field | 40–70: removes the leading all-digits scan and six branchy `parse_fixed_digits` calls (~1–1.5% of CPU) | 3 | low — E13 strict form: lowercase `z`/`t`, space-for-T, `+0200`, non-ASCII digits | `src/ts.rs`, `src/v2/parse.rs` | G1 twice (default, `SHOPT_CHUNK_BYTES=4096`); G4 on p20; unit tests mirroring `ts.rs` test module |
| 2 | Window-bounded day table: map `YYYYMMDD` key → day index for the ~24-day span `window_start−8d … window_end+1d`; out-of-table dates are just "bad ts", as today | 60–90: deletes `days_from_civil`, leap/`days_in_month` and the `div_euclid(400)` chain for every valid F1/F2 (4.92% → ~1.5%) | 2 | med — E7/E12: an F2 offset can move the UTC date across the window edge, so the table must span ±1 day plus the 23:59 max offset; validity outcome must stay "invalid copy", never `Overflow` | `src/ts.rs`, `src/v2/types.rs` (Ctx tables) | G1 twice; G4 on p20; property test over offsets 00:00–23:59 and dates at both edges |
| 3 | Dedicated `parse_ingest_f1` for the second ts of each event: shape is fixed F1 by D13, so skip the F3 digit scan and F2 branch entirely | 40–60: one of two event `parse_ts` calls becomes ~15 instructions; not additive with 1/2 | 1 | low — an invalid ingest must still be `Overflow` (exact i32 seconds), not a silently invalid copy (`parse.rs:28-38`) | `src/ts.rs`, `src/v2/parse.rs` | G1 twice; G4 on p20; a test that a malformed ingest still forces V1 fallback |
| 4 | Price parse: skip a run of `'0'`, then require ≤8 significant digits and accumulate with a length-bounded loop (checked fallback retained) | 10–30: purchases only, and `parse_price` is sub-0.5% of CPU; mostly removes per-byte `checked_mul` | 2 | low — leading zeros legal (`0250`), cap `MAX_PRICE = 10_000_000`, trailing `\r` is data (E20) | `src/v1.rs` (shared by V1 — do not change V1 semantics; add a V2-local copy), `src/v2/parse.rs` | G1 twice; G4; unit tests for `0`, `0250`, `10000000`, `10000001`, 40-digit input, `1000\r` |

Gains are estimates from the profile shares, not measurements. Ideas 1–3 overlap: total realistic saving is ~150–220 ms, not the sum.

## Details
**Shape-first dispatch (1).** Check `len == 20 && s[19] == b'Z'`, `len == 25 && (s[19]|0x10) == b'+'…`, else the F3 loop. Then for the civil part, load `s[0..8]` and `s[8..16]` word-wise plus `s[16..19]` as a 4-byte-safe slice: all loads stay inside the field the caller already delimited (`csv::split`), so there is no speculative over-read, no page-boundary assumption, and no changed behaviour on a field ending at the mmap end. Digit validation uses the classic `((x + 0x4646…) | (x + 0x4040…)) & 0x8080…` trick with a separator mask, then six 2-digit pairs are `(lo-0x30) + 10*(hi-0x30)`. This is the simdjson/SWAR lineage of branch-light ASCII validation, applied at 8-byte width — no AVX2 needed, so it is safe on the `x86-64-v3` default.

**Day table (2).** `Window::parse` already yields `start`/`end` (`ts.rs:79`). Build a 32-entry table keyed by day offset from `day_of(start) − 8`. A date inside the table needs no leap-year logic because the table only contains real dates; `month 13`, `Feb 30` and `Feb 29` outside a leap year simply miss the table and become bad timestamps. Rejecting a date one day early (e.g. `window_end + 1`, later pulled back by a negative offset) would be a correctness bug, hence the `+1`/`−1` margin. Keep `days_from_civil` for `Window::parse` itself, where arbitrary manifest dates are legal.

**F3 (all three ideas).** F3 keeps the current semantics exactly: `s.iter().all(u8::is_ascii_digit)` then `parse_digits`'s `checked_mul`/`checked_add` (`ts.rs:202`), so arbitrarily long leading-zero runs like `0000000000000000000000000000001791071890000` stay valid and `i64::MAX + 1` stays `None`. The length-independent digit test is what makes this legal; any "reject when len ≥ 20" shortcut is wrong (`ts.rs` tests `f3_leading_zeros_and_short_values`, `f3_overflow_is_none_and_never_panics`). Do **not** test every 26-digit string — 10²⁶ inputs is infeasible. Test instead: bounded exhaustive over all digit strings of length 1–6 against an independent big-integer reference, then deterministic property tests for lengths 7–64 (leading-zero runs, `i64::MAX`, `MAX+1`, 10^n boundaries) and non-digit rejection.

## Rejected ideas
- **Length ≥ 20 ⇒ invalid F3:** wrong by spec; the checked arbitrary-length loop must stay (32% of timestamps are F3).
- **Exhaustive 26-digit F3 test:** 10²⁶ cases; bounded exhaustive + property tests give the same assurance.
- **Single `u64` accumulate with wrap detection:** fails on leading-zero runs longer than 19 bytes unless a zero-skip is added first; the checked loop stays as fallback for that reason.
- **16/32-byte wide loads that cross the field end:** would rely on padding or page-rounding luck; rejected as unsafe speculative loads.
- **Memoising `parse_ts` by field hash:** hash+probe costs more than the ~20-instruction parse at 58% F1.
- **Unrolled `kind_of`:** three 4–8 byte compares, not in the top-40; no measurable headroom.
- **Product/campaign `hashbrown` lookups (5.07% `HashMap<u128,u16>::get`):** real hotspot but a dimension lookup, outside this track (see `parse-work-avoidance`/`dedup`).

## Dependencies / conflicts
Idea 1/3 touch `parse.rs` call sites that `parse-work-avoidance` may reorder or skip; land this track first or rebase. Idea 2 adds a table to `Ctx`, which `dedup` and `parse-work-avoidance` also touch via `types.rs`. Idea 4 shares `parse_price` with V1, so it must be a V2-local copy — V1 behaviour is frozen (D11). `00-profile.md`'s dedup bucket (22.21%) is a different phase; do not attribute its cost here.

## Sources
- `rules.md:24-36` (F1/F2/F3 grammar, leading zeros, bad forms, windows); `rules.md:123-124` (58/10/32% mix, 0.3–0.7% bad).
- `src/ts.rs:15` `parse_ts`, `:172` `parse_civil`, `:202` `parse_digits`, `:219` `parse_fixed_digits`, `:249` `days_from_civil`; tests `f3_leading_zeros_and_short_values`, `f3_overflow_is_none_and_never_panics`, `calendar_validation_matches_the_oracle`, `offset_can_change_the_utc_date`.
- `src/v2/parse.rs:13` `event`, `:95` `click`; `src/v1.rs:98` `parse_price`, `:28` `MAX_PRICE`, `:134` `kind_of`; `src/id.rs:10` `pack`.
- `research/perf/00-profile.md` (symbol table, 2,900 samples, 4.92% ts family, 22.21% closure#5 dedup), `research/perf/README.md` §1/§4.
- `PLAN.md:357-362` (G1 run twice, G4, G1 commands).
- H. Hinnant, "chrono-Compatible Low-Level Date Algorithms" — http://howardhinnant.github.io/date_algorithms.html (the `days_from_civil` derivation).
- D. Lemire & J. Keiser, "Parsing Gigabytes of JSON per Second" (simdjson), arXiv:1902.08318 — SWAR digit/structural validation technique.
