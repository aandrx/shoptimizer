# Serial Pass 1 profile

2026-11-21 20:29 ET, checkout 1f4e467; no diff to e6f1279 in src, Cargo files or .cargo. Native release locked, warmed CSVs via sequential read, SHOPT_IO_STATS=1 NCPU=8 taskset -c 0-3,8-11. No researchers running. One observation per size, not a median. 27 GiB host, free -g before 60M: used/free/available 12/5/14, swap 13/15 GiB used; before 20M 12/5/14 and swap 14/15. Warm attempt did not eliminate major faults. Local HT not a scorer measurement.

|Phase|20M ms|60M ms|ratio|20M min/maj faults|60M min/maj faults|
|---|---:|---:|---:|---:|---:|
|open|10|8|0.8|326/2|330/1|
|links+events|838|3425|4.09|19613/0|266424/0|
|clicks|208|572|2.75|3293/0|109605/107|
|dedup|589|2561|4.35|2087/0|402608/0|
|sessions|392|1319|3.36|7190/0|36825/0|
|write|101|354|3.50|8375/0|9846/3|
|internal total|2142|8243|3.85| | |
|native start to exit|2199|8561|3.89| | |

20M user/sys 12.251/2.059 CPU-s, 60M 40.867/12.389. VmHWM 4736628/12895600 KiB (includes mappings). End-dedup RssAnon 2855936/7243812 KiB. Wall minus internal total is NOT isolated exit teardown.

## Top 40 user-space flat sample buckets

perf record/stat unavailable. Separate serial gperftools run at 250 Hz; 8541 samples; 8479 ms internal. Decoded first PC in each legacy stack using executable mapping + ELF text bias 0x1000, llvm-nm sorted symbol extents. Names clipped to 200 chars; DSO samples grouped rather than guessed. CPU samples are not wall fractions, not kernel counters. Inlining hides callees. Closure #5 is the per-event-bucket dedup map (P2).

```
3171 37.13% 0ab1f0 <&shoptimizer::v2::run_v2<shoptimizer::run::{closure#3}>::{closure#5} as core::ops::function::FnMut<(usize,)>>::call_mut
 836  9.79% 0a5cc0 <rayon_core::job::HeapJob<<rayon_core::scope::Scope>::spawn<shoptimizer::v2::scatter<shoptimizer::v2::types::EvRaw, shoptimizer::v2::run_v2<shoptimizer::run::{closure#3}>::{closure#2}::{closure#1}>::{
 700  8.20% 0a4fd0 <rayon_core::job::HeapJob<<rayon_core::scope::Scope>::spawn<shoptimizer::v2::scatter<shoptimizer::v2::types::Ck, shoptimizer::v2::run_v2<shoptimizer::run::{closure#3}>::{closure#4}>::{closure#0}::{clo
 591  6.92% 170890 mi_page_free_list_extend
 525  6.15% libc.so.6
 502  5.88% 0bc950 memchr::arch::x86_64::memchr::memchr2_raw::find_avx2
 452  5.29% 05ccd0 core::slice::sort::unstable::quicksort::quicksort::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure
 291  3.41% 060150 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::vec::DrainProducer<usize>, rayon::iter::fold::FoldConsumer<rayon::iter::fold::FoldConsumer<rayon::iter::reduce::ReduceConsumer<rayon::i
 207  2.42% 091330 shoptimizer::ts::parse_civil
 184  2.15% 06c9a0 rayon_core::join::join_context::<rayon_core::join::join::call<core::option::Option<shoptimizer::v2::people::PersonMap>, shoptimizer::v2::run_v2<shoptimizer::run::{closure#3}>::{closure#1}>::{closure#0
 129  1.51% 0916a0 shoptimizer::ts::parse_ts
 127  1.49% 0915d0 shoptimizer::ts::days_from_civil
 112  1.31% 05c550 core::slice::sort::unstable::quicksort::quicksort::<shoptimizer::v2::types::Ck, <[shoptimizer::v2::types::Ck]>::sort_unstable_by_key<(u128, u32, u128), <shoptimizer::v2::sessions::Acc>::run_bucket::{c
  93  1.09% 056800 core::slice::sort::shared::smallsort::small_sort_general::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{
  92  1.08% 0bc750 memchr::arch::x86_64::memchr::memchr_raw::find_avx2
  72  0.84% 04a9e0 <hashbrown::raw::RawTable<(u128, u128)>>::reserve_rehash::<hashbrown::map::make_hasher<u128, u128, hashbrown::hasher::DefaultHashBuilder>::{closure#0}>
  69  0.81% 043bd0 <hashbrown::map::Entry<u128, u32, hashbrown::hasher::DefaultHashBuilder>>::or_insert_with::<shoptimizer::v2::people::from_pairs<core::iter::adapters::copied::Copied<core::iter::adapters::flatten::Flat
  50  0.59% 05fac0 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::iter::enumerate::EnumerateProducer<rayon::slice::chunks::ChunksProducer<shoptimizer::v2::types::UserRow>>, rayon::iter::map::MapConsume
  37  0.43% 09b9f0 shoptimizer::output::write_user_daily_named
  35  0.41% 044370 <hashbrown::map::HashMap<u128, u16>>::get::<u128>
  34  0.40% 0628e0 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::slice::IterProducer<&[u8]>, rayon::iter::map::MapConsumer<rayon::iter::inspect::InspectConsumer<rayon::iter::while_some::WhileSomeConsu
  32  0.37% 0561b0 core::slice::sort::shared::smallsort::small_sort_general::<shoptimizer::v2::types::Ck, <[shoptimizer::v2::types::Ck]>::sort_unstable_by_key<(u128, u32, u128), <shoptimizer::v2::sessions::Acc>::run_buc
  20  0.23% 0aab60 <alloc::vec::Vec<i64> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i64, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_u
  17  0.20% 0aa140 <alloc::vec::Vec<i32> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i32, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_u
  17  0.20% 0aad90 <alloc::vec::Vec<i64> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i64, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_u
  15  0.18% 0554a0 core::slice::sort::shared::pivot::median3_rec::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#0}>
  15  0.18% 043290 <parquet::column::writer::byte_budget_chunker::ByteBudgetChunker>::byte_budget_sub_batch::<parquet::column::writer::encoder::ColumnValueEncoderImpl<parquet::data_type::ByteArrayType>>
  11  0.13% 184100 mi_free
  11  0.13% 0aafc0 <alloc::vec::Vec<i64> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i64, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_u
  10  0.12% 09dec0 <&<shoptimizer::v2::sessions::Acc>::merge as core::ops::function::Fn<(shoptimizer::v2::sessions::Acc, shoptimizer::v2::sessions::Acc)>>::call
   9  0.11% 186280 mi_theap_malloc_aligned
   7  0.08% 184a90 mi_arenas_page_alloc_fresh
   5  0.06% 0edb90 <alloc::raw_vec::RawVecInner>::finish_grow
   5  0.06% 07f330 <parquet::column::writer::GenericColumnWriter<parquet::column::writer::encoder::ColumnValueEncoderImpl<parquet::data_type::ByteArrayType>>>::write_mini_batch
   4  0.05% 0e9830 <parquet::data_type::ByteArray as parquet::data_type::private::ParquetValueType>::encode::<alloc::vec::Vec<u8>>
   3  0.04% 17b620 _mi_page_free_collect
   3  0.04% 177ed0 mi_bbitmap_try_find_and_clearNX
   3  0.04% 055320 core::slice::sort::shared::pivot::median3_rec::<shoptimizer::v2::types::Ck, <[shoptimizer::v2::types::Ck]>::sort_unstable_by_key<(u128, u32, u128), <shoptimizer::v2::sessions::Acc>::run_bucket::{closu
   3  0.04% 0dcd40 <alloc::raw_vec::RawVecInner<_>>::reserve::do_reserve_and_handle::<alloc::alloc::Global>
   3  0.04% 186210 _mi_page_malloc_zero
```
