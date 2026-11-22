# Round 2 research index

Start: 2026-11-21T20:28:44-05:00; deadline 21:58 ET. Same machine as synthesis. Checkout 1f4e467; executable sources e6f1279.

|NN|Slug|Status|File|Researcher|Deleted|
|---|---|---|---|---|---|
|01|roofline|done|01-roofline.md|fe453501-1542-47f|N/A (ephemeral)|
|02|dedup-arch|done|02-dedup-arch.md|b523c3b1-e371-42f|N/A (ephemeral)|
|03|scan-avx2|done|03-scan-avx2.md|cd33bf0b-809d-4c8|N/A (ephemeral)|
|04|memory-traffic|done|04-memory-traffic.md|12b2c1c4-48fa-4d0|N/A (ephemeral)|
|05|sessions-60m|done|05-sessions-60m.md|43f95f3f-aa64-45a|N/A (ephemeral)|
|06|critical-path|done|06-critical-path.md|cc7435a8-9011-432|N/A (ephemeral)|
|07|input-shape|done|07-input-shape.md|0cc5d2c6-88b0-425|N/A (ephemeral)|
|08|clicks-60m|done|08-clicks-60m.md|4dbb4e10-0f28-4c7|N/A (ephemeral)|

## Track list changes
All seven seed tracks retained. Add clicks-60m: 572 ms (6.9% internal), distinct click scatter symbol at 8.2% CPU; scan handles shared field parsing but no seed owns click-specific person lookup/scatter. Memory-traffic owns mimalloc 6.9% and unresolved libc 6.1% (no attribution assumed); critical-path covers links and exit; sessions owns output overlap. No separate output card (4.3% and already covered). External deep research is assigned to the synthesis session per §6.

## Handoff to synthesis

Start with **02 (compact exact-key P2 dedup)**: 2.561 s measured phase, 37.1% flat user samples in its inlined bucket closure. Test compact payload vs table reuse separately; neither a 300–500 ms gain nor a 3 s end-to-end target is established. Follow with counted scatter and an independent person-bucket sweep (05); keep peak memory and skew in the acceptance gate. Timestamp dispatch (07) is an independent low-scope A/B. Do not add 01/04's arena gains to counted-scatter savings, or add 06/08's overlapping click proposals. Treat per-phase CPU/wall conversions, allocator gains and cache residency as models. Recheck every candidate serially at 60M with exact gates and an unmodified fallback. No evidence here for an independent 5.56 s reduction before freeze. External report and final build-card synthesis remain separate.

## Synthesis (R2-2)

Landed 21:40 ET: `research/perf2/SYNTHESIS.md` written; `deep-research.md` consolidated from the existing external notes; PLAN.md §6 Pass 2 rewritten as Pass 2′ (W1 S4-3 → W2 S4-1 → W3 S4-2/S4-4 screen → W4 S4-5, 9 h cap).

## Cleanup

Finished 20:39 ET, within 90 min. Eight researchers, eight reports, 28 idea rows with risk, verification and freeze fields. Seven tracks received one corrective pass; checked all section sets. Pi Crew researcher sessions are ephemeral (maintenance roster: persist_session false, rememberAgents false, outputTranscript false); handles above identify agent calls, not persisted session files. Checked pi --help: no session-delete CLI. Deleted persisted IDs: none. Deleted only this run's scratch/ (profile outputs and sampling script); no code/plan changes. Same-machine handoff, no commit or push.
