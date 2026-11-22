# Round-2 synthesis: alternative (Claude Code session, with deep research)

**What this file is:** the alternative to `SYNTHESIS.md`, which the other session wrote and which is conservative. The full cards are in `research/perf2/PASS2-ALT.md`. Both files use the same inputs: `00-profile.md`, pi tracks `01`–`08`, and `deep-research.md` (external; notes in `research_notes/Rust pipeline 60M scaling techniques/`).

## Where this disagrees with `SYNTHESIS.md`

| Point | `SYNTHESIS.md` (other session) | This file | Why |
|---|---|---|---|
| Thin-tuple radix dedup | Deferred: "8–12 h, high risk" | **Build it now, in parallel, as #5** (5–7 h, low–medium risk) | It's the only item worth more than 1 s (1.5–2.0 s at 60M). Exact `u128` keys in a 24 B tuple, hashbrown per partition, no unsafe code, no fingerprints. The old P2 stays behind `SHOPT_DEDUP=wide`. Supporting work: Bandle et al. (partitioning pays only at ≤ 16–32 B tuples), Balkesen et al. (3.5×), Polychroniou & Ross (10–12-bit fanout). |
| Exact-key compact dedup (W1) and flat scatter (W2) | The plan (≈ 0.35–0.7 s for 5.5 h) | Fine as the **safe #4 line**, but the thin design replaces both | The two designs rewrite the same P2, so W1 and W2 are throwaway once the thin design lands. |
| THP / huge pages | Rejected: "S3-8 already rejected; host A/B negative" | **Re-test under THP `madvise`** with a wrapper that requests huge pages itself | The laptop runs THP `always`, which hands out huge pages unasked and hides the effect. Google documents `madvise` as the GKE default. Exit teardown is ~250–320 ms at 60M (318 ms measured gap), and THP cuts the anonymous part ~10×. |
| 12.4 CPU-s system time | Not addressed | **Diagnose first (1 h)** | Faults explain only ~0.5 CPU-s. The swap-heavy laptop may be inflating it. |
| Benchmark hygiene | Pinned serial | Same, **plus** THP `madvise`, swap cleared, `flock` lock, and a p60 reference output | Without the lock, parallel worktrees can't share the laptop. |
| Expected result | ~6.5–7.0 s scored at 60M | **~5–6 s scored** if B1 lands; the #4 safe line is unchanged either way | |

## Recommended hybrid (what I'd run tonight)

1. **Line 1 (safe, → #4):** the other session's W1→W4 as written, plus A0, A1 and A2 from `PASS2-ALT.md`. A2 is adopted only if it wins under `madvise`.
2. **Line 2 (stretch, → #5):** B1 thin dedup from `e6f1279` in its own worktree, then the B2 bits A/B, then B3.
   - Line 2 doesn't need W1 or W2: it replaces P2 entirely.
   - If B1 hasn't passed G1 by Sun 03:30, drop it.
3. **Final pick (H4):** the best local 60M bench median among accepted submissions.

## Shared conclusions (both syntheses agree)

- **The 0.27 s practice leader isn't an honest pipeline.** Container start alone is 0.15–0.6 s; an honest 20M run is ~0.7–1.2 s.
- **Rejected:** a global atomic winner table, the fork or early-exit trick (PID-namespace teardown), and the full AVX2 scanner before the freeze.
- **Measurement-gated:** clicks ∥ dedup and the person-probe memo.
