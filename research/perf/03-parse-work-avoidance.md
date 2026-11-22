# 03 parse-work-avoidance

## Summary
The only sizable winner is deferring the `products.csv` category lookup to purchases: `Ev.cat` is read *only* inside `if ev.purchase` (`src/v2/sessions.rs:150-158`), so ~97% of event rows pay a `HashMap<u128,u16>` probe that no output can observe — worth ~250–400 ms of the projected 8.9 s at 60M. Every variant of "skip payload fields for duplicate copies" is capped by the dirt itself: duplicates are only 2–4% of events, and the fields of a losing copy become known only *after* the lexer has already scanned the line. The main risk is silent semantic drift in `top_category`, which a single unit test currently pins.

## Current cost
From `00-profile.md` (p20, median wall 2,962 ms) and the §1 table: event+click parse lives inside `links+events` (1,082–1,246 ms) and `clicks` (257–265 ms); the schedule is links union-find (≈250 ms, serial) overlapped with the parallel event parse, so the event parse is the long pole and parse savings land almost 1:1 in wall time. Projected 60M: `links+events` ≈3.3–3.7 s, `clicks` ≈0.8 s, total ≈8.9 s (00-profile.md: "a naive 3× estimate gives 8.89 s").

Sample shares (user CPU, same relative shape, not phase wall): `csv::split::<7>` 23.72%, the per-line `run_v2` body 22.21%, `hashbrown::map::HashMap<u128,u16>::get` **5.07%**, `ts::parse_ts`+`parse_civil`+`days_from_civil` ≈4.9%. The 5.07% figure is a *single* monomorphization shared by both `ctx.products` and `ctx.campaigns` (`types.rs:76,81` are the same type). Events outnumber clicks 4:1 (PLAN.md:357 `--events 2000000 --clicks 500000`), so the products probe is plausibly ~80% of that ≈4.1% of wall ≈ 360 ms at 60M — inference, not measurement.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Resolve `cat` only for `purchase` rows; non-purchases carry `unknown_cat` | **+250…400** (5.07% samples × 0.8 products share × 8.9 s) | 1–2 | low (E5/E13 unchanged; `top_category` unchanged) | `src/v2/parse.rs` (+ its tests) | G1 (`cargo test parse`, fixtures, seeds 2–30, 41–43 @2M), G4 `MATCH` on p20; assert purchase-row category histogram identical |
| 2 | Reorder per-copy checks by reject frequency × cost (empty user/type/empty product before `ts` parse; late check right after the window check) | **0…10** — total rejectable share is only ≈3.4% of rows (rules.md:124–133), so max avoided work is a fraction of one `parse_ts` per rejected row | 1 | low-med: can flip `Rec`→`Overflow` and trigger the V1 fallback | `src/v2/parse.rs` | G1 + forced-fallback tests; diff `[phase] links+events` only |
| 3 | Fuse a per-event `event_id → min ingest` guard into the parse loop to skip payload parsing of duplicate *losers* | **−50…+40** (net ≈0: ~2.3% of rows skippable, one extra 60M-probe global map) | 4–6 | med-high: E4 (trap: later copies name a different user), E5 | `src/v2/parse.rs`, `src/v2/mod.rs` | G1 + G4; count `Parsed::Rec` with `VALID==0` before/after |
| 4 | Keep `ingest_ts` as raw F1 bytes for the min-compare, convert to seconds once per surviving event | **+10…25** (saves ~6% of one `parse_ts` per line) | 2 | low-med (F1 is fixed-width, so byte order == time order) | `src/v2/parse.rs`, `src/v2/types.rs` | G1 seeds incl. E13 ingest-overflow case |

## Details

**1. Purchase-only category resolution.** `parse.rs:83-84` packs `product` and probes `ctx.products` for *every* valid event, but `Ev.cat` is consumed at exactly one place, `sessions.rs:150-158`, guarded by `ev.purchase`; `user_daily.top_category` is defined over purchase prices only (rules.md:83). Keep the `product` presence check required by rules.md:52 (`parse.rs:62`) — an empty `product_id` still invalidates — then:

```rust
let cat = if purchase { id::pack(product).and_then(|p| ctx.products.get(&p).copied()).unwrap_or(ctx.unknown_cat) }
          else { ctx.unknown_cat };
```

Pitfalls: (a) `parse.rs:193-196` asserts a `view` event's category is `books`; that test encodes the old semantics and must be rewritten to assert `unknown_cat` for non-purchases plus a purchase case; (b) store `unknown_cat`, never `0` (index 0 is a real category — `types.rs:101`), so any future reader stays in bounds; (c) do not touch `EvRaw.cat` for purchases, so D17's tie-break is untouched. This is per-attribute materialization, the same argument VLDB'25 SLM makes against a single blanket early/late decision (duckdb.org/library/selective-late-materialization).

**2. Frequency-ordered early reject.** Reject rates: bad user 0.2%, bad type 0.1%, bad price 0.15% of rows, empty product ~0.05%, bad ts 0.3–0.7%, late 0.5–1.5%, malformed 0.05% (rules.md:121-133). Cheapest first is tempting (`kind_of`, `parse.rs:58`, is three compares). The trap is `Parsed::Overflow`: it is only reachable *after* the earlier checks, so hoisting any `id::pack` (`parse.rs:22,49,78`) above a check that can reject turns an invalid-but-encodable row into a run-wide V1 fallback (~36 s at 20M, worse at 60M). Keep every `pack` last. Ceiling reasoning: the rejects are ~1–2% of rows *after* dedup, so this is noise — it is worth knowing that, not doing.

**3. Fused dedup guard.** 2–4% of events are duplicated with 1–3 extra copies (rules.md:121), i.e. ≈6% extra lines at d=3%, e=2. In random order the *k*-th copy is the running minimum with probability 1/k, so ≈39% of those copies could skip their payload — ≈2.3% of rows. Each skip still pays the full `split::<7>` (field count and quoting must be validated: E13, E20, rules.md "CSV dialect"), plus a global `event_id` probe that 5.07% shows is not free. Two hard requirements: a *loser* must never donate its fields (1 in 64 duplicated events has different users on later copies), and a loser being invalid says nothing about the winner (2% of duplicated events have the earliest copy's `ts` bad). Only equal-`ingest` copies are guaranteed identical (rules.md:51).

**4. Raw-bytes ingest compare.** F1 is `YYYY-MM-DDTHH:MM:SSZ`, fixed width, so lexicographic order equals chronological order; the minimum copy could be selected on raw bytes and converted only for survivors. Savings are capped by `(lines − distinct events)/lines` ≈ 6%.

## Rejected ideas
- **Parse payloads in pass 1, re-split survivors from stored offsets.** Survivors are ~97% of rows, so the lexer runs almost twice; strictly worse.
- **Skip `field()` quoting validation on unread fields (e.g. `price` for views).** A quote not wrapping the whole field makes the *line* malformed (E13/E20, `csv.rs:74-91`); semantics change.
- **Early comma-count prefilter to skip `split::<7>`.** The exact field count is itself part of malformedness, so all separators must be counted anyway.
- **Click dedup to avoid re-parsing duplicate clicks.** D7 (PLAN.md:99) already shows duplicates cannot change credit or assists; a hash pass over ~1–3% of clicks costs more than the parse it saves.
- **Deferred validation into the dedup pass (D12 relaxation).** Would force a second lexer pass over 97% of lines; also contradicts D12/dedup track 04.

## Dependencies / conflicts
- Idea 1's absolute gain **shrinks** if track 12 (dimension-lookup) makes the products probe cheaper, and its *share* grows if tracks 01/02 make `split::<7>`/`parse_ts` faster. Ideas 1–4 are mutually non-additive (same loop).
- Idea 3 overlaps track 04 (dedup) and track 05 (scatter) and should only proceed if 04's measurements justify an in-parse probe.
- Idea 4 overlaps track 02 (same `parse_ts` calls) — hand it to 02 rather than duplicating.
- No idea changes §4 decisions D6/D8/D12/D13; idea 1 narrows the blanket phrasing of §5a O5 (PLAN.md:174), which should be noted but is a playbook item, not a decision.

## Sources
- `rules.md:50-53` (Events 1–4, dedup-before-validation), `rules.md:83` (`top_category` = purchase prices), `rules.md:121-133` (dirt rates), `rules.md` "CSV dialect"/E13/E20 traps.
- `PLAN.md:99` (D7 no click dedup), `PLAN.md:104` (D12 validity at parse), `PLAN.md:173-174` (O4, O5), `PLAN.md:357,360` (G1, G4), `PLAN.md:337` (≥3% median win or revert).
- `src/v2/parse.rs:22,26,49,52,58,62,75,78,83-88,193-196`; `src/v2/sessions.rs:150-158`; `src/v2/types.rs:29,39,76,81,101,118`; `src/v2/mod.rs:139-160`; `src/csv.rs:57-91`; `src/ts.rs:15-41`; `src/id.rs:9-17`.
- `research/perf/00-profile.md` (phase medians, sample shares), `research/perf/INDEX.md`.
- Liu, Tang, Hui, Zhou, Zhang, *Selective Late Materialization in Modern Analytical Databases*, VLDB 2025 — https://duckdb.org/library/selective-late-materialization/ (retrieved 2026-11-21; abstract confirms per-attribute materialization points; 14.7%/8.9% averages over EM/LM on JOB).
- Uncertainty: no measurement of the products-vs-campaigns split of the 5.07% sample share, no per-row reject counts at p20, and no timing taken during this fan-out (per §0 rule 3).
