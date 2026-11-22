# Perf research index
Run started: 2026-11-21T18:54:23-05:00   Base commit: 79635d9561b2da444de3e2ff464e5c54437aaa16 (code bf0d6d5)   Machine: same as synthesis

| NN | Slug | Status | File | Researcher session ID | Deleted |
|---|---|---|---|---|---|
| 01 | simd-csv | done | 01-simd-csv.md | wf_2a535f5104a1 / research:1 | N/A (ephemeral) |
| 02 | field-parsing | done | 02-field-parsing.md | wf_2a535f5104a1 / research:2; retry 9a2db889-dddc-43f | N/A (ephemeral) |
| 03 | parse-work-avoidance | done | 03-parse-work-avoidance.md | wf_2a535f5104a1 / research:3 | N/A (ephemeral) |
| 04 | dedup | done | 04-dedup.md | wf_2a535f5104a1 / research:4 | N/A (ephemeral) |
| 05 | partition-scatter | done | 05-partition-scatter.md | wf_2a535f5104a1 / research:5 | N/A (ephemeral) |
| 06 | sort-sessionize | done | 06-sort-sessionize.md | wf_2a535f5104a1 / research:6 | N/A (ephemeral) |
| 07 | links-unionfind | done | 07-links-unionfind.md | wf_2a535f5104a1 / research:7 | N/A (ephemeral) |
| 08 | io-memory | done | 08-io-memory.md | wf_2a535f5104a1 / research:8 | N/A (ephemeral) |
| 09 | parquet-output | done | 09-parquet-output.md | wf_2a535f5104a1 / research:9 | N/A (ephemeral) |
| 10 | microarch | done | 10-microarch.md | wf_2a535f5104a1 / research:10 | N/A (ephemeral) |
| 11 | pipeline-shape | done | 11-pipeline-shape.md | wf_2a535f5104a1 / research:11 | N/A (ephemeral) |
| 12 | dimension-lookup | done | 12-dimension-lookup.md | wf_2a535f5104a1 / research:12 | N/A (ephemeral) |

## Track list changes vs seed (§3)

See [00-profile.md](00-profile.md). At 23.72% of samples for split<7> alone, CSV keeps its own track. Dedup, sort, scatter and lookup need separate bounds. Split dictionary lookup out of field parsing: HashMap<u128,u16>::get alone is 5.07% flat. Merge runtime-startup into microarch (thread count/pool) and io-memory (allocator/page faults): open was 9–11 ms, under 0.4% of native total; Docker startup not measured. The 8.69% unresolved libc group is not a single identified hotspot: io-memory/partition-scatter must bound it, not attribute it blindly. Keep links, I/O and materialization for 60M scale despite overlap. Twelve researcher tracks, cap fifteen, no fan-out timing.

## Validation and handoff

The sampled p20 output matched the cached V1 oracle (2,242,393 user rows; 28,014 campaign rows; zero duplicate keys). No proposed optimization was implemented or benchmarked. Estimates are hypotheses: do not sum gains from overlapping work. In particular, the 22.21% bucket closure is **dedup**, not newline search. Read the validation notes at the top of the affected reports before using their estimates. Highest-priority inexpensive experiment: avoid product category lookup on non-purchases, preserving whole-line validation, nonempty product and any required fallback checks. Reprofile before combining scanner, dedup, and layout ideas.

## Cleanup

The session column gives the workflow task label, **not** a persisted Pi session ID. This roster uses `persist_session: false`, `rememberAgents: false`, and `outputTranscript: false`; no saved researcher conversations to delete. `pi --help` lists no session-delete CLI. Deleted session IDs: none (not created). Do not delete the parent session, unrelated sessions, or confuse workflow bookkeeping with researcher conversations. Scratch deleted: yes (only this run’s `research/perf/scratch/`). Track 02 was rerun once for structure and F3 correctness. All 12 reports have seven required sections and four idea rows each. No tracked code or plan edits. An unrelated untracked `research_notes/` appeared during the run and was left untouched. Same-machine handoff: no commit or push.
