# 07 input-shape

## Summary

Measured generator geometry (seed 923: 158,381 sampled event rows, 23,490 click rows) leaves only two shape-gated timestamp paths worth building, a hypothesis of ~0.7–1.0 CPU-s saved at 60M for 2.5 h. The "quote-free line" fast path is not worth it: it covers 21.0% of sampled event and 40.9% of click lines, and its gate costs about what it saves. No-locality follows from generator source, not from sampling.

*No timing was run. Every gain below is a hypothesis derived from code structure, instruction counts and CPU-sample shares, never a measured wall delta.*

## Current cost

Shape-sensitive work lives inside `links+events` (3,425 ms measured at 60M, `research/perf2/00-profile.md`) and `clicks` (572 ms). Two different profiles must not be mixed:

- **Current code (Pass 1, `00-profile.md`, 8,541 CPU samples):** `memchr2_raw::find_avx2` 5.88%, `memchr_raw::find_avx2` 1.08%, and timestamps `parse_civil` 2.42% + `parse_ts` 1.51% + `days_from_civil` 1.49% = **5.42%**. These are CPU samples, not wall.
- **Round 1 (`research/perf/00-profile.md`) describes the pre-Pass-1 scalar `csv::split`**, whose 27.1% share is obsolete; `src/v2/split.rs` now finds syntax bytes with `memchr2` and validates quotes while walking (`split.rs:14-72`). Do not reuse that 27% as today's scanning cost.

At 60M, user CPU is 40.867 s (`00-profile.md`). 5.42% of it is ≈ **2.2 CPU-s** in timestamps. How much of the 8.24 s wall that is worth is not measurable from a flat CPU profile; the parallel-efficiency divisor is unknown here, so this report stops at CPU-s and labels every wall figure a hypothesis.

Measured coverage. The sample conditions on expected comma count for all shapes except quote-free, strips quotes for shape purposes only (regex is not semantic validation), lowercases `user` (so case variants count as repeats), and takes ≤32 consecutive rows per block start, which biases toward row clusters.

| Shape | Events (sampled) | Clicks (sampled) | Structural expectation |
|---|---|---|---|
| Quote-free line | 33,282/158,381 = 21.0% | 9,597/23,490 = 40.9% | 0.8⁷ = 20.97%, 0.8⁴ = 40.96% (`plan.rs:140` "20% of fields", `rules.md:11-12`) |
| `event_id` length 9 / 8 / 7 / ≤6 / empty | 92.8 / 6.94 / 0.18 / 0.006 / 0.017% | — | `"e"` + base36 of a 2⁴⁰-bijection (`user.rs:121-124`, `text.rs:192,211`); ~93% of 2⁴⁰ needs 8 base36 digits |
| `user_id` length 9 / 8 / 7 / empty | 93.0 / 6.64 / 0.17 / 0.21% | 93.2 / 6.60 / 0.18 / 0.20% | same construction; empty at `bad_user` 0.2% (`plan.rs:133`) |
| `ts` F1 / F2 / F3 / other | 56.3 / 9.2 / 34.1 / 0.39% | 57.4 / 9.9 / 32.4 / 0.30% | 60/10/30 draws (`text.rs:99-107`) plus ms-start sessions |
| `event_type` view / cart / purchase / bad | 87.1 / 9.79 / 2.99 / 0.10% | — | `user.rs:333-342`, `plan.rs:136` |
| `price_cents` empty / 3–5 digits / other | 68.3 / 27.2 / 2.3% | — | non-purchase 70% empty, purchase 3% (`user.rs:365-383`) |

Two corrections to inherited assumptions. First, F3 is not 32% per row: any timestamp with milliseconds can only be F3 (`text.rs:101`) and a session's millisecond start carries to every event in that session (`user.rs:40`, `:219-300`), so ~32.7% per row, plus cluster amplification in the sample (34.1%) — the rules' "about 58/10/32" (`rules.md:123`) is approximate, and design should treat F3 as ≥32%. Second, "all IDs ≤ 9 bytes" is a property of *this* generator, not a guarantee: `id::pack` accepts up to 15 bytes and anything longer forces the V1 fallback (`src/id.rs:10-19`, `PLAN.md:98`), so a length-keyed fast path must keep that check.

## Ideas

Every gain is a hypothesis from static structure and CPU-sample shares; none was measured.

| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | **Shape-first `ts` dispatch**: test F1 (`len==20 && s[19]==b'Z'`) then F2 (`len==25 && (s[19]==b'+' \|\| s[19]==b'-') && s[22]==b':'`) before the all-digits F3 scan, then reuse `parse_civil`; every unmatched shape falls through to today's body | hypothesis 0.5–0.8 CPU-s at 60M: removes the failed all-digits pre-scan (`ts.rs:16-19`) from ~65% of rows and reorders nothing semantically. Wall effect unmeasured | 1.5 | low — E13 strict form (lowercase `z`/`t`, space-`T`, `+0200`), E20 `\r` is data, non-ASCII digits; branch order must stay disjoint from all-digit F3 | `src/ts.rs` (additive), `src/v2/parse.rs` | differential `dispatch(s) == parse_ts(s)`: exhaustive over 10 symbols `{0,1,9,-,:,T,Z,+,.,space}` up to length 6 (1.1M strings) + 1M random up to 40 bytes + the `ts.rs` edge tests; G1 all three configs; G4 `MATCH` on `data/p20` | none; narrower V2-scoped sibling of round-1 02 #1 | **Yes** |
| 2 | **Fixed-width F3 + dedicated ingest F1**: `if s.len()==13` load 8+5 bytes, SWAR digit check, accumulate; parse the event's second `ts` with a 20-byte F1 parser | hypothesis 0.1–0.2 CPU-s: 34% of `ts` fields currently scan 13 bytes twice (`all()` then `parse_digits`), and 60M ingest fields pay an F1/F3 dispatch they can never win; but no fast path here is more than a few percent of `links+events` | 1 | low — F3 leading zeros, unbounded length and `i64::MAX+1` must still return `None`; malformed ingest must stay `Parsed::Overflow`, not an invalid copy (`parse.rs:28-38`) | `src/ts.rs`, `src/v2/parse.rs` | same differential harness, plus `0001791071890000`, 31 leading zeros, `9223372036854775808`; G1 all three configs; G4 | none | **Yes** |
| 3 | **Row-size constants from measured shape**: `bytes_per_row` 70→~78 events, 45→~49 clicks in `prealloc_cap` | ≈0 ms hypothesis: `Vec::with_capacity` does not fault untouched pages, and the current +15% headroom already covers the mean (mean line length itself is an estimate, not measured) | 0.2 | none | `src/v2/mod.rs:50-53` | no perf assertion; G1 all three configs; G4 | S3-5 pre-sizing | Yes, but not worth a card (Rejected) |

## Details

**Disjointness of idea 1.** An F3 value is all digits, so a 20-byte all-digit string fails `s[19]==b'Z'` and a 25-byte one fails `s[22]==b':'`; an F1/F2 value is never all digits. Shape-first testing therefore cannot reclassify any input, and the fallback is the current body unchanged.

**The F2 sign test must be an explicit `||`.** `+` is `0x2B` and `-` is `0x2D`; `0x2D | 0x10 = 0x3D = '='`, so a bitwise-OR trick is wrong here. Copy `src/ts.rs:24-25` verbatim, including the `hours > 23 || minutes > 59` rejection and `-00:00` as UTC.

**Why idea 2 stays narrow.** F3 must keep accepting leading zeros and arbitrary length (`ts.rs` tests `f3_leading_zeros_and_short_values`, `f3_overflow_is_none_and_never_panics`), so the 13-byte path is an accelerator keyed on `len` with the checked loop as fallback. SWAR digit masks follow the standard `(x + 0x4646…) | (x + 0x4040…)` form (Lemire §4), applied to loads that stay inside the already-delimited field.

**Quote accounting.** One quoted field contributes **two** quote bytes, so an event line averages 2.8 quote bytes (0.2 × 7 fields × 2). No claim is made here about quote density per 32-byte block or about any block-level fast path: the sample measures lines, not blocks, and a block-level estimate would need its own measurement.

## Rejected ideas

- **Line-level quote-free `split` fast path**: the gate scans the whole line, paid by the 79% of event lines that do contain a quote, while the saving on the 21% that do not is a few avoided `memchr2` hits. Net hypothesis ≈ 0 ± 40 ms; a `memchr`-only splitter is also not reliably faster than `memchr2` on AVX2.
- **`≤8`-byte `id::pack` u64 fast path**: covers 6.9% of rows and `id::pack` has no profile presence; a `≤9` version would be a guess about dataset shape, and 15-byte IDs must keep working.
- **`kind_of` reordering**: already view-first, matching the 87% share (`v1.rs:134-139`).
- **Price parse fast path**: only purchases parse price (2.99% of rows); hypothesis ceiling well under 30 ms.
- **Duplicate-locality dedup shortcut**: dead (below).
- **Pre-size constants (idea 3)**: no fault or traffic saving.
- **Any shape assumption without a runtime check** ("F3 is 13 digits", "IDs are 9 bytes", "no `\r`"): excluded by `research/perf2/README.md` §0 rule 3.

## No-locality: confirmed from source

Every emitted line draws its part independently per copy (`Row::emit` → `parts.pick(random)`, `user.rs:611,648,653`), and each part file is then fully shuffled (`main.rs:182-202`) across 24–160 event parts (`plan.rs:58`). Duplicate copies and a person's rows are scattered by construction. The sampled repeat counts (event IDs 54 present / 108 absent against the first sampled part, users 38,025 / 1,022) are overlap statistics, not pair-placement evidence, and carry the cluster bias above; treat the source as proof and those counts as corroboration only.

## Dependencies / conflicts

- Ideas 1–2 touch `src/ts.rs`, shared with V1 (`PLAN.md:341`): additive only, with V1 continuing to call `parse_ts` unchanged.
- Gains land in the parallel parse inside `links+events`, which overlaps the serial union-find (`mod.rs:168-182`); if that tail later exceeds the parse wall, late savings are hidden. At 3.4 s vs an estimated ~0.75 s tail there is room.
- Round-1 02 #2 (day table) is complementary and excluded; if both land, idea 1's dispatch is its natural host.
- No overlap with S4-1..S4-4. A future AVX2 scanner would supersede `split`, not these edits.

## Sources

- `rules.md:20-22` dialect; `:24-35` F1/F2/F3 and "ingest_ts is always valid F1" (`:34`); `:117-133` dirt table (quoted fields 20%); `:11-12` columns.
- `.tools/generator/src/text.rs:99-107` `valid_timestamp` (60/10/30, ms→F3 at `:101`, F2 at `:103`), `:84` `write_utc` (ingest is F1), `:192` `base36`, `:211` `scramble`; `user.rs:40` `MILLISECOND_START_SHARE`, `:121-124` ID construction, `:219-300` layout, `:333-342` event types, `:365-383` price field, `:611-655` emit/part choice; `plan.rs:8,58,128-141` (rates incl. `quoted_field: 0.2` at `:140`); `main.rs:182-202`.
- `src/ts.rs:15-45` `parse_ts` (F3 first at `:16-19`, F1 `:20-22`, F2 `:24-25`), `:172` `parse_civil`, `:202` `parse_digits`, `:249` `days_from_civil`; `src/v2/parse.rs:13-89`; `src/v2/split.rs:14-72`; `src/id.rs:10-19`; `src/v2/mod.rs:50-53`; `src/v1.rs:98-139`.
- `PLAN.md:98` (D6: ≤15-byte pack, longer falls back), `:176` O7, `:341` shared-code rule, `:555` (AVX2 scanner deferred), `:566-569` G1/G4.
- `research/perf2/00-profile.md` (60M phases, CPU-s, sample shares); `research/perf/00-profile.md` and `research/perf/02-field-parsing.md` (pre-Pass-1 shares, day-table idea).
- H. Hinnant, "chrono-Compatible Low-Level Date Algorithms": https://howardhinnant.github.io/date_algorithms.html ; D. Lemire, "Number Parsing at a Gigabyte per Second": https://arxiv.org/abs/2101.11408 .

## Recommended card

**R2-S5 — Shape-first timestamp parse** · Scope: `src/ts.rs` (additive), `src/v2/parse.rs` · Depends: none · 2.5 h · est. **hypothesis** 0.6–1.0 CPU-s at 60M (unmeasured wall).

- **Change:** route each `ts` field through a new `ts::parse_shape` that tries F1 (`len==20 && s[19]==b'Z'`), then F2 (`len==25 && (s[19]==b'+' || s[19]==b'-') && s[22]==b':'`), then today's all-digit F3 body verbatim; parse the event's `ingest_ts` with a dedicated 20-byte F1 parser; add a `len==13` SWAR path in front of the F3 loop. `parse_ts` is unchanged and remains the oracle.
- **Accept:** G1 in all three configs (default, `SHOPT_CHUNK_BYTES=4096`, `SHOPT_BUCKET_BITS=1 SHOPT_CHUNK_BYTES=100`); G4 `MATCH` on `data/p20`; a differential test asserting `parse_shape(s) == parse_ts(s)` for every string up to length 6 over `{b'0', b'1', b'9', b'-', b':', b'T', b'Z', b'+', b'.', b' '}` (≈1.1M inputs), plus 1M random strings up to 40 bytes and the existing `ts.rs` edge cases: `0001791071890000`, a 31-leading-zero run, `9223372036854775808`, `2026-10-03T23:58:10z`, `2026-10-03T23:58:10+05:60`, `2026-10-03T23:58:10-00:00`, `2026-10-03T23:58:10.500Z`, and a trailing `\r`.
- **Traps:** E13/E20 form strictness; F3 leading zeros and unbounded length must keep working; a malformed `ingest_ts` must still produce `Parsed::Overflow` (V1 fallback), never a silently invalid copy; F2 offsets can move the UTC date across a window edge (E7/E12); never assume 13-digit F3, 9-byte IDs or quote-free input without the runtime check; expect the wall gain to be smaller than the CPU gain and partly hidden by the union-find tail.
