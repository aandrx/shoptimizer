# 00 Serial p20 profile

2026-11-21, Ryzen 7 8840HS. Research checkout `79635d9`, executable sources identical to `bf0d6d5`. No researchers or concurrent benchmark jobs. `cargo build --release`; existing `data/p20`; `SHOPT_ENGINE=v2 NCPU=8 taskset -c 0-3,8-11 target/release/shoptimizer data/p20 research/perf/scratch/profile/out`. CPU sibling pairs verified as 0/1, 2/3, 8/9, 10/11. No cache drop or claimed cold start.

| Native serial run | open | links+events | clicks | dedup | sessions | write | internal total | wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
|1 (ms)|11|1082|259|811|444|129|2794|2826|
|2|9|1229|257|807|428|147|2940|2976|
|3|9|1246|265|744|476|125|2924|2962|

Median wall 2962 ms. A naive 3× estimate gives 8.89 s at 60M locally, **not measured**. Links and events overlap: do not add isolated savings. Percentages below are user CPU samples, not phase wall fractions; inlined operations can be charged to their callers. Container startup is not measured.

## Symbol sample

Neither `perf record` nor `perf stat` was installed. Retrieval of a matching signed portable perf package failed, so instead of omitting symbols a separate serial run used gperftools `LD_PRELOAD=/usr/lib/libprofiler.so CPUPROFILE_FREQUENCY=250 CPUPROFILE=...`; 2,900 sampled stacks, 2.816 s wall. No PMU counters or kernel time attribution. Decoded legacy CPU profile flat leaf PCs using its mappings, executable ELF LOAD bias (`+0x1000` for its executable segment), demangled `nm -nSC` ranges. libc grouped as unresolved, not evidence for any one libc function. Top 40 buckets; addresses identify long monomorphizations.

```text
count   flat%  linked address and symbol
 688 23.72% 0x4e320 shoptimizer::csv::split::<7>
 644 22.21% 0xa2990 <&shoptimizer::v2::run_v2::{closure#5} as core::ops::function::FnMut<(usize,)>>::call_mut
 252  8.69% libc.so.6 (unresolved DSO samples)
 239  8.24% 0x5d860 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::slice::IterProducer<&[u8]>, rayon::iter::fold::FoldConsumer<rayon::iter::extend::ListVecConsumer, shoptimizer::v2::run_v2::{closure#2}::{closure#0}, shoptimizer::v2::run_v2::{closure#2}::{closure#1}>>
 147  5.07% 0x42330 <hashbrown::map::HashMap<u128, u16>>::get::<u128>
 133  4.59% 0x5e410 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::slice::IterProducer<&[u8]>, rayon::iter::fold::FoldConsumer<rayon::iter::extend::ListVecConsumer, shoptimizer::v2::run_v2::{closure#3}, shoptimizer::v2::run_v2::{closure#4}>>
 101  3.48% 0x5b1d0 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::range::IterProducer<usize>, rayon::iter::fold::FoldConsumer<rayon::iter::fold::FoldConsumer<rayon::iter::reduce::ReduceConsumer<rayon::iter::ParallelIterator::reduce_with::opt_reduce<shoptimizer::v2::sessions::Acc, &<shoptimizer::v2::sessions::Acc>::merge>::{closure#0}, <core::option::Option<shoptimizer::v2::sessions::Acc> as core::default::Default>::default>, <core::option::Option<shoptimizer::v2::sessions::Acc> as core::default::Default>::default, rayon::iter::ParallelIterator::reduce_with::opt_fold<shoptimizer::v2::sessions::Acc, &<shoptimizer::v2::sessions::Acc>::merge>::{closure#0}>, shoptimizer::v2::run_v2::{closure#6}, shoptimizer::v2::run_v2::{closure#7}>>
  95  3.28% 0x4dfb0 shoptimizer::csv::split::<4>
  93  3.21% 0x166bf0 mi_page_free_list_extend
  92  3.17% 0x598d0 core::slice::sort::unstable::quicksort::quicksort::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#0}>::{closure#0}>
  52  1.79% 0x8ca70 shoptimizer::ts::parse_civil
  50  1.72% 0x8cd10 shoptimizer::ts::days_from_civil
  43  1.48% 0x53640 core::slice::sort::shared::smallsort::small_sort_general::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#0}>::{closure#0}>
  41  1.41% 0x8cde0 shoptimizer::ts::parse_ts
  26  0.90% 0x7aa70 <parquet::column::writer::GenericColumnWriter<parquet::column::writer::encoder::ColumnValueEncoderImpl<parquet::data_type::ByteArrayType>>>::write_mini_batch
  25  0.86% 0x59150 core::slice::sort::unstable::quicksort::quicksort::<shoptimizer::v2::types::Ck, <[shoptimizer::v2::types::Ck]>::sort_unstable_by_key<(u128, u32, u128), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#1}>::{closure#0}>
  16  0.55% 0x68620 rayon_core::join::join_context::<rayon_core::join::join::call<core::option::Option<shoptimizer::v2::people::PersonMap>, shoptimizer::v2::run_v2::{closure#1}>::{closure#0}, rayon_core::join::join::call<alloc::vec::Vec<alloc::vec::Vec<alloc::vec::Vec<shoptimizer::v2::types::EvRaw>>>, shoptimizer::v2::run_v2::{closure#2}>::{closure#0}, core::option::Option<shoptimizer::v2::people::PersonMap>, alloc::vec::Vec<alloc::vec::Vec<alloc::vec::Vec<shoptimizer::v2::types::EvRaw>>>>::{closure#0}
  16  0.55% 0x5a7c0 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::iter::enumerate::EnumerateProducer<rayon::slice::chunks::ChunksProducer<shoptimizer::v2::types::UserRow>>, rayon::iter::map::MapConsumer<rayon::iter::try_reduce::TryReduceConsumer<rayon::iter::ParallelIterator::try_for_each::ok<core::result::Result<(), std::io::error::Error>>, <() as core::default::Default>::default>, shoptimizer::v2::write_users::{closure#0}>>
  15  0.52% 0x52ff0 core::slice::sort::shared::smallsort::small_sort_general::<shoptimizer::v2::types::Ck, <[shoptimizer::v2::types::Ck]>::sort_unstable_by_key<(u128, u32, u128), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#1}>::{closure#0}>
  12  0.41% 0x68450 <hashbrown::raw::RawTable<u64>>::reserve_rehash::<<parquet::util::interner::Interner<parquet::encodings::encoding::dict_encoder::KeyStorage<parquet::data_type::ByteArrayType>>>::intern::{closure#1}>::{closure#0}
  10  0.34% 0x880a0 <parquet::column::writer::GenericColumnWriter<parquet::column::writer::encoder::ColumnValueEncoderImpl<parquet::data_type::Int64Type>>>::write_mini_batch
  10  0.34% 0x95e90 shoptimizer::output::write_user_daily_named
   8  0.28% 0x4dc70 shoptimizer::csv::split::<2>
   7  0.24% 0x17c590 _mi_page_malloc_zero
   6  0.21% 0xa18e0 <alloc::vec::Vec<i32> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i32, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_user_daily_named::{closure#1}>>>::from_iter
   5  0.17% 0x97790 <&<shoptimizer::v2::sessions::Acc>::merge as core::ops::function::Fn<(shoptimizer::v2::sessions::Acc, shoptimizer::v2::sessions::Acc)>>::call
   5  0.17% 0x168b40 mi_usable_size
   5  0.17% 0x52420 core::slice::sort::shared::pivot::median3_rec::<shoptimizer::v2::types::Ev, <[shoptimizer::v2::types::Ev]>::sort_unstable_by_key<(u128, u32), <shoptimizer::v2::sessions::Acc>::run_bucket::{closure#0}>::{closure#0}>
   4  0.14% 0x17c190 mi_find_page
   4  0.14% 0xa2300 <alloc::vec::Vec<i64> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i64, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_user_daily_named::{closure#2}>>>::from_iter
   3  0.10% 0x17d020 mi_realloc_aligned
   3  0.10% 0x17a480 mi_free
   3  0.10% 0xa2760 <alloc::vec::Vec<i64> as alloc::vec::spec_from_iter_nested::SpecFromIterNested<i64, core::iter::adapters::map::Map<core::slice::iter::Iter<shoptimizer::output::UserDaily>, shoptimizer::output::write_user_daily_named::{closure#4}>>>::from_iter
   3  0.10% 0x772f0 <alloc::raw_vec::RawVecInner>::finish_grow
   3  0.10% 0x5d100 rayon::iter::plumbing::bridge_producer_consumer::helper::<rayon::slice::IterProducer<&[u8]>, rayon::iter::map::MapConsumer<rayon::iter::inspect::InspectConsumer<rayon::iter::while_some::WhileSomeConsumer<rayon::iter::extend::ListVecConsumer>, <core::option::Option<_> as rayon::iter::FromParallelIterator<core::option::Option<_>>>::from_par_iter::check<alloc::vec::Vec<(u128, u128)>>::{closure#0}>, shoptimizer::v2::people::build::{closure#0}>>
   3  0.10% 0x7ff20 <parquet::column::writer::GenericColumnWriter<parquet::column::writer::encoder::ColumnValueEncoderImpl<parquet::data_type::Int32Type>>>::write_mini_batch
   2  0.07% 0x1719a0 _mi_page_free_collect
   2  0.07% 0x17ae10 mi_arenas_page_alloc_fresh
   2  0.07% 0xd3540 <alloc::raw_vec::RawVecInner<_>>::reserve::do_reserve_and_handle::<alloc::alloc::Global>
   2  0.07% 0x179b10 mi_abandoned_page_try_reclaim
```
