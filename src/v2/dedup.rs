//! Dedup kernels for V2 P2 (B1).
//!
//! `dedup_wide` is the original per-bucket `HashMap<u128, EvRaw>` reducer, kept
//! as the `SHOPT_DEDUP=wide` path and as the test oracle. `dedup_thin` consumes
//! the 24-byte [`Thin`] tuples built in P1: one exact-key hash map per dedup
//! bucket, loser bitmaps, then a streaming person scatter in record blocks.

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;
use rayon::prelude::*;

use crate::id;

use super::people::PersonMap;
use super::types::{Ev, EvRaw, Thin, PURCHASE, VALID};

/// The low 32 key bits hold `worker << 28 | offset`, so both fields are bounded.
pub const MAX_WORKERS: usize = 16;
pub const OFFSET_LIMIT: usize = 1 << 28;

/// P2b block size in records: enough to amortize the per-block count vectors.
const BLOCK: usize = 1 << 20;

/// Pack `(ingest_s, worker, offset)` for [`Thin::key`].
///
/// `ingest_s ^ 0x8000_0000` makes the unsigned high half sort like `i32`.
/// `None` means V1 fallback (offset or worker count too large).
#[inline]
pub fn key(ingest_s: i32, worker: usize, offset: usize) -> Option<u64> {
    if worker >= MAX_WORKERS || offset >= OFFSET_LIMIT {
        return None;
    }
    let ingest = ((ingest_s as u32) ^ 0x8000_0000) as u64;
    Some((ingest << 32) | ((worker as u64) << 28) | offset as u64)
}

#[inline]
fn lost(bitmaps: &[Vec<u64>], worker: usize, offset: usize) -> bool {
    bitmaps[worker][offset >> 6] & (1u64 << (offset & 63)) != 0
}

/// Original P2: dedup per event bucket, then exact-size person buckets.
pub fn dedup_wide(
    ev_parts: &[Vec<Vec<EvRaw>>],
    people: &PersonMap,
    bits: u32,
    person_bits: u32,
) -> Vec<Vec<Vec<Ev>>> {
    let nb = 1usize << person_bits;
    (0..(1usize << bits))
        .into_par_iter()
        .map(|b| {
            let total: usize = ev_parts.iter().map(|p| p[b].len()).sum();
            let mut kept: HashMap<u128, EvRaw> = HashMap::with_capacity(total);
            for part in ev_parts {
                for r in &part[b] {
                    match kept.entry(r.id) {
                        Entry::Occupied(mut o) => {
                            // Equal ingest_ts copies are identical: either is fine.
                            if r.ingest_s < o.get().ingest_s {
                                o.insert(*r);
                            }
                        }
                        Entry::Vacant(v) => {
                            v.insert(*r);
                        }
                    }
                }
            }
            // Count VALID survivors per destination person bucket, so each output
            // Vec gets its exact capacity and never doubles (S3-5).
            let mut counts = vec![0usize; nb];
            for r in kept.values_mut() {
                if r.flags & VALID == 0 {
                    continue;
                }
                r.user = people.person(r.user);
                counts[id::bucket(r.user, person_bits)] += 1;
            }
            let mut out: Vec<Vec<Ev>> = counts.into_iter().map(Vec::with_capacity).collect();
            for r in kept.values() {
                if r.flags & VALID == 0 {
                    continue;
                }
                out[id::bucket(r.user, person_bits)].push(Ev {
                    person: r.user,
                    ts: r.ts,
                    price: r.price,
                    cat: r.cat,
                    purchase: r.flags & PURCHASE != 0,
                });
            }
            out
        })
        .collect()
}

/// B1 P2: thin exact-key dedup + streaming person scatter.
pub fn dedup_thin(
    mut recs: Vec<Vec<EvRaw>>,
    thin: Vec<Vec<Vec<Thin>>>,
    people: &PersonMap,
    nd: usize,
    person_bits: u32,
) -> Vec<Vec<Vec<Ev>>> {
    // One reusable map and one task-local loser Vec per rayon fold state.
    let max_partition = (0..nd)
        .map(|b| thin.iter().map(|p| p[b].len()).sum::<usize>())
        .max()
        .unwrap_or(0);
    let losers: Vec<u32> = (0..nd)
        .into_par_iter()
        .fold(
            || {
                (
                    HashMap::<u128, u64>::with_capacity(max_partition),
                    Vec::<u32>::new(),
                )
            },
            |(mut map, mut losers), b| {
                map.clear();
                for part in &thin {
                    for t in &part[b] {
                        match map.entry(t.id()) {
                            Entry::Vacant(v) => {
                                v.insert(t.key);
                            }
                            Entry::Occupied(mut o) => {
                                let old = *o.get();
                                if t.key < old {
                                    losers.push(old as u32);
                                    o.insert(t.key);
                                } else {
                                    losers.push(t.key as u32);
                                }
                            }
                        }
                    }
                }
                (map, losers)
            },
        )
        .map(|(_, losers)| losers)
        .reduce(Vec::new, |mut a, b| {
            a.extend_from_slice(&b);
            a
        });

    // S3-3: the thin tuples are dead now.
    std::thread::spawn(move || drop(thin));

    let mut bitmaps: Vec<Vec<u64>> = recs
        .iter()
        .map(|r| vec![0u64; r.len().div_ceil(64)])
        .collect();
    for low in losers {
        let worker = (low >> 28) as usize;
        let offset = (low & ((1 << 28) - 1)) as usize;
        bitmaps[worker][offset >> 6] |= 1u64 << (offset & 63);
    }

    // Disjoint mutable slices of the immutable per-worker record buffers.
    let mut blocks: Vec<(usize, usize, &mut [EvRaw])> = Vec::new();
    for (worker, part) in recs.iter_mut().enumerate() {
        blocks.extend(
            part.chunks_mut(BLOCK)
                .enumerate()
                .map(|(i, chunk)| (worker, i * BLOCK, chunk)),
        );
    }

    let nb = 1usize << person_bits;
    let ev_out: Vec<Vec<Vec<Ev>>> = blocks
        .into_par_iter()
        .map(|(worker, start, block)| {
            // Pass 1: resolve survivor people, count exact person buckets.
            let mut counts = vec![0usize; nb];
            for (i, r) in block.iter_mut().enumerate() {
                if lost(&bitmaps, worker, start + i) || r.flags & VALID == 0 {
                    continue;
                }
                r.user = people.person(r.user);
                counts[id::bucket(r.user, person_bits)] += 1;
            }
            let mut out: Vec<Vec<Ev>> = counts.into_iter().map(Vec::with_capacity).collect();

            // Pass 2: fill them. The person is already cached in `r.user`.
            for (i, r) in block.iter().enumerate() {
                if lost(&bitmaps, worker, start + i) || r.flags & VALID == 0 {
                    continue;
                }
                out[id::bucket(r.user, person_bits)].push(Ev {
                    person: r.user,
                    ts: r.ts,
                    price: r.price,
                    cat: r.cat,
                    purchase: r.flags & PURCHASE != 0,
                });
            }
            out
        })
        .collect();

    // S3-3: the parse buffers are dead now.
    std::thread::spawn(move || drop(recs));
    ev_out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::v2::people::from_pairs;

    fn rec(id: u128, user: u128, ingest_s: i32, flags: u8) -> EvRaw {
        EvRaw {
            id,
            user,
            ingest_s,
            ts: 1_000,
            price: 250,
            cat: 3,
            flags,
        }
    }

    fn thin_of(recs: &[Vec<EvRaw>], nd: usize, bits: u32) -> Vec<Vec<Vec<Thin>>> {
        recs.iter()
            .enumerate()
            .map(|(worker, part)| {
                let mut out: Vec<Vec<Thin>> = (0..nd).map(|_| Vec::new()).collect();
                for (offset, r) in part.iter().enumerate() {
                    let key = key(r.ingest_s, worker, offset).unwrap();
                    out[id::bucket(r.id, bits)].push(Thin {
                        id_lo: r.id as u64,
                        id_hi: (r.id >> 64) as u64,
                        key,
                    });
                }
                out
            })
            .collect()
    }

    fn wide_of(recs: &[Vec<EvRaw>], nd: usize, bits: u32) -> Vec<Vec<Vec<EvRaw>>> {
        recs.iter()
            .map(|part| {
                let mut out: Vec<Vec<EvRaw>> = (0..nd).map(|_| Vec::new()).collect();
                for r in part {
                    out[id::bucket(r.id, bits)].push(*r);
                }
                out
            })
            .collect()
    }

    fn counts(ev_out: &[Vec<Vec<Ev>>]) -> BTreeMap<(u128, u32, u32, u16, bool), usize> {
        let mut out = BTreeMap::new();
        for ev in ev_out.iter().flatten().flatten() {
            *out.entry((ev.person, ev.ts, ev.price, ev.cat, ev.purchase))
                .or_insert(0) += 1;
        }
        out
    }

    #[test]
    fn key_bounds_and_order() {
        assert_eq!(key(0, MAX_WORKERS, 0), None);
        assert_eq!(key(0, 0, OFFSET_LIMIT), None);
        assert!(key(0, MAX_WORKERS - 1, OFFSET_LIMIT - 1).is_some());
        // Unsigned key order follows signed ingest_s, then worker/offset.
        assert!(key(-5, 9, 9).unwrap() < key(-1, 0, 0).unwrap());
        assert!(key(-1, 0, 0).unwrap() < key(0, 0, 0).unwrap());
        assert!(key(0, 0, 1).unwrap() < key(0, 1, 0).unwrap());
    }

    #[test]
    fn thin_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Thin>(), 24);
    }

    #[test]
    fn thin_matches_wide_reference() {
        const BITS: u32 = 2;
        const PERSON_BITS: u32 = 2;
        const ND: usize = 1 << BITS;
        let (a, b, c, d, e, f, g) = (11u128, 22, 33, 44, 55, 66, 77);

        // Duplicate groups of 2, 3 and 4 copies, spread across workers.
        // D has equal ingest (identical copies). E's earliest copy is invalid,
        // so the event must stay dropped (E5). F's later copy names another
        // user, but the winning copy's user must be used (E4).
        let recs = vec![
            vec![
                rec(a, 100, 5, VALID),
                rec(b, 110, 9, VALID),
                rec(c, 120, 19, VALID),
                rec(e, 140, 40, 0),
                rec(f, 150, 50, VALID),
            ],
            vec![
                rec(a, 100, 7, VALID),
                rec(b, 110, 12, VALID),
                rec(c, 120, 21, VALID),
                rec(d, 130, 30, VALID),
                rec(e, 140, 42, VALID),
                rec(g, 170, 60, VALID),
            ],
            vec![
                rec(b, 110, 10, VALID),
                rec(c, 120, 20, VALID),
                rec(d, 130, 30, VALID),
                rec(e, 140, 43, VALID),
                rec(f, 160, 55, VALID),
            ],
            vec![rec(c, 120, 22, VALID)],
        ];

        let people = from_pairs(std::iter::empty(), 0);
        let thin = dedup_thin(
            recs.clone(),
            thin_of(&recs, ND, BITS),
            &people,
            ND,
            PERSON_BITS,
        );
        let wide = dedup_wide(&wide_of(&recs, ND, BITS), &people, BITS, PERSON_BITS);
        assert_eq!(counts(&thin), counts(&wide));

        // Exactly a, b, c, d, f, g survive; e is dropped and f keeps user 150.
        let got = counts(&thin);
        assert_eq!(got.values().sum::<usize>(), 6);
        assert!(!got.keys().any(|k| k.0 == 140), "E5 winner was invalid");
        assert!(got.contains_key(&(100, 1_000, 250, 3, false)));
        assert!(got.contains_key(&(150, 1_000, 250, 3, false)));
        assert!(!got.keys().any(|k| k.0 == 160), "later F copy must lose");
    }
}
