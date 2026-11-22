# Round-2 synthesis (R2-2)

Written 2026-11-21 ~21:30 ET by the synthesis session from: the eight fan-out reports (`00-profile.md`, `01`–`08`), the consolidated external evidence (`deep-research.md`, from the five notes in `research_notes/Rust pipeline 60M scaling techniques/`), and the budget audit. Research-only restrictions applied to the fan-out; this document and the Pass 2′ rewrite of `PLAN.md` §6 are the synthesis deliverables. No code has changed.

## Measured starting point

`e6f1279` (#3) at 60M, pinned serial, warm cache: **8.24 s internal / 8.56 s process wall** (single runs, ±10% host noise; scored expectation ~8.5–9 s). Phases: links+events 3.43 s, dedup 2.56 s, sessions 1.32 s, clicks 0.57 s, write 0.35 s. Dedup scales 4.35× for 3× input and takes ~403k fresh-page faults; its inlined bucket closure is 37.1% of flat CPU samples. Top single symbols after that: event scatter 9.8%, click scatter 8.2%, `mi_page_free_list_extend` 6.9%, `memchr2` 5.9%, `Ev` sort 5.3%, timestamps ~4.5%. Round-1's 27% split share no longer applies (S3-4 replaced the splitter).

## Ranked build order (renumbered Pass 2′ in PLAN.md §6)

| Wave | Card | Change | Hypothesis at 60M | Cap |
|---|---|---|---|---|
| W1 | S4-3 | Exact-key map to dense winner payloads in P2 | 250–400 ms | 3 h |
| W2 | S4-1 | Flat counted person scatter (reuse count pass + cached person) | 100–300 ms | 2.5 h |
| W3 | S4-2 | `SHOPT_DEDUP_BITS` / `SHOPT_PERSON_BITS` split | vs (8,8) screen | 1 h |
| W3 | S4-4 | Partition screen (8,8)→(10,8)→(10,11), defaults only | vs (8,8) screen | 0.5 h |
| W4 | S4-5 | Full validation, Docker, seeds, 60M record | — | 2 h |

Total 9 h including gates. S4-3 first: largest measured phase, lowest-risk mechanism, and W2 builds on its count pass. W2 may start from #3 if W1 is rejected, but the two rewrite the same P2 output — never parallel, never both half-landed.

**Non-additivity (do not sum hypotheses):** W1 and W2 both shrink P2's traffic; 01/04 arena ideas overlap them; 06/08 overlap proposals compete for the same cores. The only honest total is the pinned 60M wall.

## Key corrections the synthesis accepts

- The P2 closure does **one** person probe per valid survivor (cached in the count pass, reused in fill). Reports that assumed two were corrected.
- `memchr2` is already AVX2; the splitter is not the 27% it was in round 1. The fused scanner (03) is a 6–10 h rewrite for ~300–700 ms and stays out of Pass 2′.
- CPU sample shares are not wall fractions; per-phase utilization was never measured, so overlap gains (06, 08) are unproven. Their cards are measurement-gated and excluded from the budget.
- Person-map memo (08) has a break-even: only pays above a measured ~5–15% exact-key repeat rate; gate before building.
- No dataset-specific shortcuts (rule 3); no env-default allocator experiments after S3-8's negative A/B; no fork tricks (deep-research: PID-namespace teardown defeats them).

## Dropped from the old Pass 2

- **S4-3 as originally written** (fingerprint slots, packed worker/offset indices, 6–8 h): replaced by the exact-key compact payload (02 #1), which captures most of the traffic win at a third of the risk and hours. Fingerprint slots remain an optional stage 2.
- **S4-4 sub-partition code** (05): the per-bucket rank pass costs more than it saves; replaced by the default screen.
- **External "big levers"** (deep-research): thin-tuple radix dedup (~1.8–2.2 s, 8–12 h, high risk) and fused AVX2 scanner belong to a Submission #5, not tonight.

## Deferred candidates (measurement-gated, only if W1/W2 land early and validation passes)

1. Clicks inside dedup (06/08): only with a measured idle/overlap screen.
2. Person-probe memo (08): only with a measured ≥ 15% exact-key repeat rate.
3. Timestamp shape-first dispatch + fixed-width F3 (07 #1/#2): 2.5 h, ~0.6–1.0 CPU-s, independent of P2.
4. Arena-backed P2 output (01/04): only if fault counts stay high after W1/W2.

## Reality check on the 3 s goal

The roofline track puts an honest design floor at ~1.6–2.0 s local and a realistic post-optimization target at ~2.4–3.0 s at 60M — but every number above ~4 s reduction depends on unmeasured mechanisms (scanner rewrite, radix dedup). What survives the 9 h budget (`00-profile` + W1–W4) plausibly lands **7.8 s → ~6.5–7.0 s scored**, not 3 s. The practice leader's 0.27 s is not an honest-full-pipeline comparison (rule 3).

## Validation contract (unchanged from S3-9)

G1 all configs (after W3, tiny = `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`), G4 vs cached p20 V1, G3 before/after, G2 Docker, 60M self-checks + seeds 51–53 + sampled anon ≤ 10 GB. Stop optimizing by Sun 04:30 ET.
