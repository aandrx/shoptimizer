# 12 dimension-lookup

> **Orchestrator validation note (supersedes conflicting claims below):** A uniqueness test over known dimension keys is insufficient: an unknown input can alias a stored 64-bit signature. Verify the full ID on every candidate hit, including dense-ID prefix, zero padding, holes and bounds. Require unknown-alias negative tests before G1/G4.

## Summary
The 5.07% `HashMap<u128,u16>::get` symbol is almost entirely the per-event product→category probe (`src/v2/parse.rs:83`), with the per-click campaign probe (`parse.rs:102`) a smaller second site. A build-time pattern-detected dense array (product ID `p<decimal>` → `u16` category, ~100 KB, 50,001 entries) removes most of that probe while keeping the generic hash map as the fallback. Expected 60M wall gain is ~180–300 ms of ~8.9 s (≈2–3.5%) locally; the risk is a representation bug that silently assigns a wrong category rather than falling back.

## Current cost
Measured: 08–… `research/perf/00-profile.md` top-40 flat leaf samples put `<hashbrown::map::HashMap<u128, u16>>::get` at **5.07%** (147/2,900 samples) of a 2.816 s NCPU=8 run. There are exactly two call sites with that map type: `ctx.products.get(&p)` in the event parser (`src/v2/parse.rs:83`) and `ctx.campaigns.get(&c)` in the click parser (`src/v2/parse.rs:102`); `types.rs:126` also probes a campaign map but only ~2,000 times at build.

Volume reasoning: `products.csv` has 50,000 rows, `campaigns.csv` 2,000 (`.tools/generator/src/plan.rs:8-9`); event rows ≈ 1.06×E because 2–4% of events get 1–3 extra copies (`plan.rs:129`, generator/src/user.rs:384-386), and ~97% of copies survive all per-copy checks before reaching the lookup (bad user 0.2%, bad type 0.1%, bad ts 0.3–0.7%, late 0.5–1.5%, bad price 5% of the ~3% purchases — `plan.rs:129-142`). So ~20.6M product lookups at E=20M and ~62M at 60M; clicks are 5M→15M, ~96% valid, so ~14.4M campaign lookups at 60M. Product is ≈78% of lookups but has the larger table (50k keys ≈ 1.2 MB with control bytes), i.e. L2 misses on Cascade Lake's 1 MB/core L2; the campaign table (~48 KB) already fits L2.

Projection: 5.07% × 2,962 ms ≈ **150 ms wall-equivalent at 20M** (this is a CPU-sample share, not a phase-timing measurement), ~450 ms at 60M, ~510 ms on the 1.13× slower scorer. The lookup executes even when the product is unknown (0.5% of events), so miss handling is on the hot path, not an edge case.

## Ideas

| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (h) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Product: build-time-detected dense `Vec<u16>` indexed by the decimal suffix of `p<digits>`, default-filled with `unknown_cat`; keep the `HashMap` for any non-conforming `products.csv` | 180–300. Removes ~75–85% of the product share (0.78 × 0.8 × 450 ≈ 280; lower bound if L2 latency is better hidden by OoO) | 3–5 | med (WRONG-CATEGORY trap: unknown must map to `unknown`; leading zeros; array must default to `unknown_cat`, not 0) | `types.rs`, `parse.rs` | `cargo test parse` + new fallback/leading-zero tests, G1 (fixtures 36/36, diff-seeds 2-30, 41-43), G4 `MATCH` on p20 |
| 2 | Campaign: same detection for `c_<digits>` → 2,001-entry `u16` array (~4 KB, L1) | 20–45. Campaigns are 22% of lookups but already L2-resident; saving ~8–10 ns of ~14 at ~14.4M lookups / ~4 cores ≈ 30 | 1–2 | low–med (same traps; `organic` is not in the file) | `types.rs`, `parse.rs` | as #1, plus a click with an unknown campaign (A6 already covers) |
| 3 | Generic shim: `HashMap<u64,u16>` keyed by a 64-bit fold of the packed ID, accepted only if build-time injection check shows no truncation collision | 60–120 alone; ~0 if #1 is done (same samples) | 1–2 | low (injectivity proved at build; otherwise keep u128 map) | `types.rs`, `parse.rs` | unit test that a colliding `products.csv` keeps the u128 map; G1/G4 |
| 4 | L1-resident direct-mapped memo (512 × 18 B) in front of whichever table wins, keyed on a cheap hash of the packed value | 25–50 on top of #1. Zipf weights 1/(rank+1) (`plan.rs:252`) put ~60% of product mass in the top 512 of 50k | 1–2 | low (pure memoisation; must compare full key before trusting a slot) | `parse.rs` | as #1 plus a test that a memo slot collision returns the array value |

## Details

**1. Dense ordinal array (primary).** In `Ctx::build` (`types.rs:95-119`), after building `product_map`, scan the same `product_rows` keys once and try to interpret each as `b'p'` + 1–5 ASCII digits with no leading zero (unless the field is exactly `"p0"`). If *all* keys conform and the maximum value is ≤ a cap (e.g. 2M entries = 4 MB), replace the map with `cats: Vec<u16>` of length max+1, initialised to `unknown_cat` and filled in file order (keeping "later lines win", `types.rs:105-115`). At `parse.rs:82`, validate the raw product field bytes instead of `id::pack`-ing it: `p` prefix, 1–5 digits, no leading zero, value < len ⇒ `cats[v]`, else `unknown_cat`. This is exactly a dictionary-encoded dimension (O5, `PLAN.md:174`) with the dictionary in ordinal instead of hash form. Pitfalls: (a) the array **must** default to `unknown_cat` — `sessions.rs:39,153` sizes `cat_sum` by `categories.len()` and indexes it by `Ev.cat`, so a 0-filled default would either mis-attribute revenue to `categories[0]` or index out of bounds; (b) `Ctx::build` currently returns `None` (V1 fallback) only for IDs > 15 bytes (`types.rs:105`, `id.rs:10-13`), so a 6-byte product like `p50000` always packs — the array path must not change that; (c) if `products.csv` mixes `p1` and `p01`, the two keys collide on value 1, so the no-leading-zero rule (or a duplicate-value check) must reject the pattern and keep the map. This is a runtime representation choice over whatever the file contains, not a hard-coded dataset assumption (`PLAN.md:446`, "nothing about a dataset can be hard-coded"). Handling of the 0.5% unknown products (`rules.md:130`, including `P123` case variants and `p60000`) is unchanged: any non-conforming field yields `unknown_cat`, which is what the map miss did.

**2. Campaign array.** Same helper, prefix `c_`, 2,000 rows → 2,001 × 2 B. Because `click` must *drop* a click whose campaign is absent (`rules.md:62`), the array's default must be a sentinel (e.g. `u16::MAX`) and the parser returns `Parsed::Skip` on it, not `unknown_cat`. Small effort, small but positive gain.

**3. 64-bit-key map.** `id::pack` puts the length in the lowest byte (`id.rs:7-17`), so short IDs share the low 64 bits. Truncating to a u64 is only safe after proving at build that all packed keys are distinct as u64; then the table halves and the key compare is one 8-byte compare. This is the fallback that still helps datasets the `pN` pattern misses, but it saves roughly the same samples as #1, so it is not additive with it.

**4. Memo.** A 512-entry cache in front of the array: index by `(packed >> 100) & 511` or `id::mix`, compare the stored full key. Top-512 product mass ≈ H(512)/H(50000) ≈ 6.8/11.4 ≈ 60%, and 9 KB stays in L1 where the 100 KB array does not.

## Rejected ideas
- **Defer product→category resolution until after dedup** (store a u16 product ordinal in `EvRaw.cat`, resolve on survivors): saves only the ~3% duplicate copies' lookups (≈12 ms at 60M) and costs a new encoding plus a 65,534-product cap; it is also a conflict surface with the dedup track.
- **Sort-based or binary-search lookup on packed keys**: 16 compares of 16-byte keys per row, strictly worse than one probe.
- **Batch/prefetch software pipelining of the probe**: 4–6 h touching the whole parse loop for ~30–50 ms, and it is worth ~0 once the array (#1) is in place (no long-latency miss left to hide). Keep as the fallback plan if #1 is rejected.
- **Per-worker copies of the table**: single-threaded read-only access already; duplication would only hurt caches.
- **Moving the product lookup earlier to allow skipping other fields**: validity does not depend on product membership (`rules.md:52`), and dedup must still see every copy (`rules.md:51`), so reordering cannot remove work.

## Dependencies / conflicts
- **Field parsing (track 02)**: #1 replaces `id::pack(product)` + hash with a digit parse over the same field slice; the two changes touch the same lines `parse.rs:78-84`. Decide jointly whether the field scanner yields the digit value directly (one pass) so #1 does not add a second scan.
- **Dedup (04) / parse-work-avoidance (03)**: if dedup is fused into parse, a duplicate copy could skip the lookup entirely, making the ~3% deferral idea moot; conversely, skip-parse must not skip the product field for the *kept* copy, since `cat` is needed.
- **SIMD-CSV (01)**: unaffected; the lookup is after field splitting.
- Conflicts with nothing in the fixed constraints; only `types.rs`/`parse.rs` change, no new dependency.
- Non-additivity: #1, #3 and #4 all remove time from the same 5.07% sample group; #1+#2 is the only pair whose gains are close to additive, and #1+#3 is not additive at all.

## Sources
- `research/perf/00-profile.md` — 5.07% flat for `<hashbrown::map::HashMap<u128, u16>>::get` (147/2,900 samples, 2.816 s NCPU=8 run); phase table (median wall 2962 ms).
- `src/v2/parse.rs:78-84` (product probe), `:99-104` (campaign probe and skip-on-unknown).
- `src/v2/types.rs:73-84` (map fields), `:95-119` (product map build, later-lines-win), `:120-136` (campaign build).
- `src/v2/sessions.rs:39,80-92,153-157` — `cat` is used as an index into a `categories.len()`-sized array.
- `src/id.rs:7-17` (packing: length byte in the low byte), `:35-43` (mix).
- `rules.md:14-15` (products/campaigns shape), `:51-52` (dedup before validation; product only needs to be non-empty), `:56` (unknown → `unknown`), `:62` (click needs campaign in file), `:110-111`, `:130-131` (50,000 products / 2,000 campaigns, unknown rates).
- `PLAN.md:174` (O5 dictionary encoding at parse), `:109` (D17 category index order), `:357-360` (G1/G4), `:423-440` (trap list), `:446` (no hard-coded dataset facts).
- `.tools/generator/src/plan.rs:8-9,129-142,252` (cardinalities, duplicate and dirt rates, Zipf weights 1/(rank+1)).
- hashbrown 0.17 default hasher is foldhash: <https://docs.rs/hashbrown/0.17.0/hashbrown/struct.HashMap.html> ("The default hashing algorithm is currently foldhash"); foldhash description: <https://github.com/orlp/foldhash> — so the probe, not the hash, dominates the 5.07%.
- Cache-latency figures used above (L1 4–5 cycles, L2 ~14, L3/LLC higher on Cascade Lake) are standard engineering approximations, **not measured here**; a 7-cpu.com lookup failed through the web gateway, so treat the ns-per-lookup numbers (25–35 ns CPU) as inference from the sample share, and re-derive them with `perf stat` before committing.
