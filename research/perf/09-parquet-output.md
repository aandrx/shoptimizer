# 09 parquet-output

## Summary
The best low-risk win is per-column writer properties: disable dictionary encoding for `user_id` and page-level statistics/offset indexes, which the profile shows the crate spending time on despite the checker ignoring metadata. Expected 60M wall gain is only ~60–130 ms local (~0.7–1.5%), because P4 is ~4% of the run. A hand-written fixed-schema PLAIN writer could remove most of the ~390 ms phase but costs 8–12 h and carries real format-compliance risk.

## Current cost
Measured (20M, native serial profile, `00-profile.md:5-8`): write 129/147/125 ms, median 129 ms, total median wall 2962 ms. The write phase is one Parquet part per rayon thread (`src/v2/mod.rs:209-212`), so user_daily writes in parallel; `campaign_daily` is built and written serially afterwards (`src/v2/mod.rs:199,232-257`).

Projection (hypothesis, not measured): 117,038 user rows per 1M events (`PLAN.md:59`) gives ~2.34M rows at 20M and ~7.02M at 60M, so ~3× the rows and ~390 ms (range 375–440 ms). Against the naive 8.89 s 60M estimate (`00-profile.md:11`) that is ~4.4%; on the scorer (1.13×) ~10.0 s total, so an infinite-speedup write phase can save at most ~440 ms of wall.

Where it goes, from user-CPU samples (`00-profile.md:33,36,38,39,40,43,48,54`): `GenericColumnWriter<ByteArrayType>::write_mini_batch` 0.90%, dictionary `Interner` rehash 0.41%, Int64 0.34%, Int32 0.07%, `write_user_daily_named` plus its `Vec<i32>`/`Vec<i64>` collections another ~0.6%. The writer is configured with compression only (`src/output.rs:85-95`), so crate defaults apply: dictionary enabled (`properties.rs:39`), page statistics + column/offset index (`properties.rs:45,56,70`), 1 MiB data-page limit, 20,000 rows/page, 1M rows/row-group (`properties.rs:31,43,49`). Dictionary falls back to PLAIN at 1 MiB (`column/writer/mod.rs:1157-1160`), which for ~9-byte IDs is ~80k distinct values per part. Every row additionally allocates twice: `ByteArray::from(&[u8])` copies into a fresh `Vec` (`data_type.rs:293-297`, field `Option<Bytes>` at `data_type.rs:178-180`), used for `user_id` and `top_category` (`src/output.rs:97-98,145-151`), and each numeric column is re-materialized via `collect::<Vec<_>>()` (`src/output.rs:122,128,134,142`).

Conflict: `PLAN.md:184` says ~7M rows ≈150 MB uncompressed; PLAIN accounting (13 B user_id + 4 + 3×8 + nullable category) gives ~50 B/row, ~350 MB. No output size was measured here; treat 150 MB as unverified.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk (none/low/med/high + which trap) | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Per-column `WriterProperties`: dictionary off for `user_id`, `EnabledStatistics::None`, offset index off, fewer/larger pages; keep dictionary on `top_category`/`channel`/`campaign_id` | 60–130 ms: removes interner (0.41% of serial CPU ≈12 ms at 20M, ~35 ms at 60M) plus stats/index/page-header work and a share of `write_mini_batch`; 15–30% of a 390 ms phase | 1–2 | Low. Metadata-only; schema/nullability/`DATE` unchanged. Trap: none in §9 (tie/null semantics live in `UserRow`, not the writer) | `src/output.rs:85-95` | G1 (`cargo test`, 36/36 fixtures, `diff-seeds` including zero-row and 2M seeds) + G4 `make oracle`/`make bench` MATCH |
| 2 | Zero-copy strings: one `bytes::Bytes` arena per part, `ByteArray::from(Bytes::slice)`; write `UserRow` fields straight into reused column buffers, dropping the intermediate `UserDaily` Vec | 40–90 ms: 7M×2 small mallocs (~25 ns each) ≈ 350 ms CPU / 8 threads ≈ 45 ms, plus one 49 MB/part copy and cache pressure | 2–4 | Low. Byte-identical values; must not UTF-8-normalize. Needs dependency approval (`bytes`, already compiled via parquet) | `src/output.rs:97-151`, `src/v2/mod.rs:204-230`, `Cargo.toml` | Same G1 + G4; plus a round-trip test comparing bytes with `id::unpack` |
| 3 | Hand-written Parquet: fixed schema, PLAIN only, no stats/dictionary/index, one row group per part, compact-Thrift headers/footer | 220–300 ms: phase floor ~80–150 ms (PLAIN buffer fill + `write_all` of ~350 MB across 8 threads); saves 55–75% of 390 ms (~2.5–3.4% of 8.9 s) | 8–12 | High. Schema/def-level bugs (null `top_category`), Thrift field IDs, `DATE` logical type; empty tables must still carry the exact schema (`src/output.rs:242-300` test) | new `src/v2/write.rs` or `src/output.rs` branch | G1 all, G4 MATCH, explicit reader round-trip against the crate's `SerializedFileReader`; fixture with zero rows |
| 4 | Overlap: emit one part per completed bucket during P3 (or at least start `write_campaigns` concurrently) | 0–50 ms: same threads do the work; only tail imbalance is recovered. Non-additive with 1–3 | 4–6 | Medium. File-count churn only; names/count are free (`rules.md:72`) | `src/v2/mod.rs:196-257`, `src/v2/sessions.rs` merge | G1 + G4, plus assertion no duplicate `(user_id,day)` |

## Details
**1 — writer properties.** Add `set_column_dictionary_enabled(ColumnPath::from("user_id"), false)` (`properties.rs:1316`), `set_statistics_enabled(EnabledStatistics::None)` (`properties.rs:1194`), `set_offset_index_disabled(true)` (`properties.rs:867`), and raise `set_data_page_row_count_limit` (`properties.rs:774`) / page size limit so a ~875k-row part has ~5 pages instead of ~44. The documented reason to disable dictionary is that the checker reads values, not metadata; the crate already admits all-unique columns defeat dictionaries by falling back at 1 MiB, but only after paying hashing and a dictionary page. Keep dictionary for the three low-cardinality string columns, where it shrinks pages.

**2 — allocation-free materialization.** `ByteArray` is `Option<Bytes>`-backed, so `ByteArray::from(Bytes)` (`data_type.rs:313-316`) avoids the per-row `Vec` copy that `From<&[u8]>` performs. Build one region per part holding unpacked IDs contiguously (about 63 MB), then hand out slices; replace `bytes(rows.iter()...)` (`src/output.rs:97-98`) and the category loop (`src/output.rs:145-151`). Also skip the `Vec<UserDaily>` hop by iterating `types::UserRow` in `src/v2/mod.rs:211-227`. Pitfall: `Bytes::slice` keeps the arena alive per file; free after `close()`.

**3 — hand writer.** PLAIN INT32/INT64/BYTE_ARRAY are trivial (length-prefixed little-endian; Parquet spec "Plain" section). The work is Thrift compact `FileMetaData`/`SchemaElement`/`ColumnMetaData`/`PageHeader`, RLE/bit-packed def levels for nullable `top_category`, and a correct footer. This is exactly the "fall back to a hand-written writer" contingency in `PLAN.md:448`, so it is sanctioned but should follow cheaper ideas.

**4 — overlap.** `acc.users` is already a flat `Vec<UserRow>` after the sessions reduce, so per-bucket writing needs either per-bucket parts or a streaming channel. Benefit is bounded by P3's tail; treat as an experiment for track 11, not a standalone win.

## Rejected ideas
- Output compression: already rejected with reasoning (`PLAN.md:184`); space is not scored, CPU is.
- Dictionary for all string columns: helps `top_category`/`channel`/`campaign_id` only; `user_id` is ~all-unique (idea 1 keeps it selectively).
- INT32 instead of INT64 counts: ~56 MB smaller output but no measurable encode win, and unbounded-counter overflow risk.
- Single-part `user_daily`: loses the 8-way parallel write (`src/v2/mod.rs:209-212`).
- Adding `BufWriter`: parquet already emits ~1 MiB pages, so syscall count is small; unverified benefit.
- Compression-level or `PARQUET_2_0` encoding tuning: DELTA/RLE cost more CPU than PLAIN here.

## Dependencies / conflicts
- Track 11 (pipeline shape): if write is overlapped or done per bucket, idea 4 and much of 1–3's wall value disappear.
- Track 06 (sort/sessionize): changes `UserRow` production order; ideas must not assume ordering.
- I/O track 08: write throughput depends on page-cache state and `/output` disk; a cold-cache scorer shifts idea 3's floor upward.
- Idea 2 needs a `Cargo.toml` dependency (`bytes`), i.e. allow-list approval; ideas 1 and 3 need none.
- All ideas must keep V1 untouched as the G4 oracle (`PLAN.md:103`).

## Sources
- `research/perf/00-profile.md:5-11,33-54` — measured write phase and writer symbol samples.
- `src/v2/mod.rs:196-230` — P4 entry, part count/naming, per-row `UserDaily` construction.
- `src/output.rs:85-98,110-151` — `open_writer` properties, `ByteArray` collection, per-column `Vec` collects, nullable category.
- `src/v2/types.rs:53-62` — `UserRow` layout.
- `src/id.rs:20-26` — `unpack` into a 16-byte stack buffer (basis for the arena).
- `rules.md:70-92` — output contract: any integer width/string encoding, `DATE` required, file count/order free.
- `PLAN.md:59,103,184,448`; `PLAN.md` §7 G1/G4, §9 trap list.
- parquet crate 60.1.0 (`Cargo.toml:12`): `properties.rs:30-70` defaults, `:774,867,1194,1316` setters; `column/writer/mod.rs:1157-1160` dictionary fallback; `data_type.rs:178-180,285-300,313-316` `ByteArray` allocation semantics.
- https://docs.rs/parquet/60.1.0/parquet/file/properties/struct.WriterProperties.html (fetched; documents the builder, including `set_encoding`, `set_column_encoding`, compression).
- https://parquet.apache.org/docs/file-format/data-pages/encodings/ (fetched; PLAIN byte layout, dictionary fallback rule, RLE/bit-packed hybrid).
- https://github.com/apache/arrow-rs — parquet-rs source of the above (version pinned at 60.1.0).
