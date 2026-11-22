//! P2 exact-key dedup (S4-3, rules.md Events 2-5).
//!
//! One exact-key table maps the full event id to a dense winner index, and the
//! payloads live in a compact `Vec` that grows only on a new id. A duplicate's
//! payload is never appended: on a strictly smaller `ingest_s` the existing
//! winner is overwritten in place. The index is a checked `usize` into that
//! `Vec`; nothing is packed or truncated into it.

use hashbrown::hash_map::Entry;
use hashbrown::HashMap;

use crate::id;

use super::people::PersonMap;
use super::types::{Ev, EvRaw, PURCHASE, VALID};

/// Exact id -> dense winner index. `rows[i].id` is the key of index `i`.
pub struct Winners {
    index: HashMap<u128, usize>,
    rows: Vec<EvRaw>,
}

impl Winners {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            index: HashMap::with_capacity(cap),
            rows: Vec::with_capacity(cap),
        }
    }

    /// Insert one copy. A new id appends a payload; a duplicate only overwrites
    /// the existing winner when its `ingest_s` is strictly smaller (equal-ingest
    /// copies are interchangeable, rules.md Events 2).
    #[inline]
    pub fn insert(&mut self, r: EvRaw) {
        match self.index.entry(r.id) {
            Entry::Occupied(o) => {
                let i = *o.get();
                if r.ingest_s < self.rows[i].ingest_s {
                    self.rows[i] = r;
                }
            }
            Entry::Vacant(v) => {
                let i = self.rows.len();
                self.rows.push(r);
                v.insert(i);
            }
        }
    }

    /// Number of surviving ids (one payload each).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn rows(&self) -> &[EvRaw] {
        &self.rows
    }

    /// Mutable payloads for the person write-back pass; the map is untouched.
    pub fn rows_mut(&mut self) -> &mut [EvRaw] {
        &mut self.rows
    }
}

/// One dedup task's flat, person-partitioned output (S4-1).
///
/// `evs` is a single contiguous `Vec<Ev>`; destination person bucket `b` owns
/// the disjoint range `offsets[b]..offsets[b + 1]`. The offsets are a checked
/// prefix sum of that task's per-bucket counts.
pub struct Flat {
    evs: Vec<Ev>,
    offsets: Vec<usize>,
}

impl Flat {
    /// The records for person bucket `b`; an empty bucket is an empty slice.
    #[inline]
    pub fn bucket(&self, b: usize) -> &[Ev] {
        &self.evs[self.offsets[b]..self.offsets[b + 1]]
    }

    /// `nb + 1` range starts: `offsets[0] == 0`, `offsets[nb] == evs.len()`.
    #[cfg(test)]
    pub fn offsets(&self) -> &[usize] {
        &self.offsets
    }
}

/// Dedup one event bucket across every worker's part, then re-partition the
/// valid winners by person bucket.
///
/// Validation runs only on the winner (rules.md Events 3, E5), and the winner's
/// user is resolved to a person exactly once (E4) before the capacity count, so
/// the fill pass needs no second lookup.
///
/// `None` only if the per-bucket offsets overflow `usize` (not representable, so
/// the caller falls back to V1).
pub fn partition_bucket(
    b: usize,
    parts: &[Vec<Vec<EvRaw>>],
    people: &PersonMap,
    bits: u32,
    nb: usize,
) -> Option<Flat> {
    let total: usize = parts.iter().map(|p| p[b].len()).sum();
    let mut winners = Winners::with_capacity(total);
    for part in parts {
        for r in &part[b] {
            winners.insert(*r);
        }
    }
    // One pass over the valid winners: resolve each user to its person exactly
    // once (E4) and count the survivors per destination person bucket (S3-5).
    let mut counts = vec![0usize; nb];
    for r in winners.rows_mut() {
        if r.flags & VALID == 0 {
            continue;
        }
        r.user = people.person(r.user);
        counts[id::bucket(r.user, bits)] += 1;
    }
    // Checked prefix sum into the range starts of one flat output Vec.
    let mut offsets: Vec<usize> = Vec::with_capacity(nb + 1);
    offsets.push(0);
    let mut end = 0usize;
    for &c in &counts {
        end = end.checked_add(c)?;
        offsets.push(end);
    }
    // Fill that one Vec: every winner is written straight into its bucket's
    // disjoint range, so the P3 gather reads contiguous slices.
    let mut evs: Vec<Ev> = Vec::with_capacity(end);
    let mut cur = offsets[..nb].to_vec();
    {
        let spare = evs.spare_capacity_mut();
        for r in winners.rows() {
            if r.flags & VALID == 0 {
                continue;
            }
            let dest = id::bucket(r.user, bits);
            let i = cur[dest];
            cur[dest] = i + 1;
            spare[i].write(Ev {
                person: r.user,
                ts: r.ts,
                price: r.price,
                cat: r.cat,
                purchase: r.flags & PURCHASE != 0,
            });
        }
    }
    // SAFETY: the count pass counted exactly the rows written above, and each
    // bucket's cursor only advances inside its own `counts[b]`-sized range, so
    // every slot in `0..end` is written exactly once. `Ev` is `Copy` with no
    // `Drop`, so a panic before this point leaks nothing.
    unsafe { evs.set_len(end) };
    Some(Flat { evs, offsets })
}

/// The pre-S4-3 P2 reducer, verbatim: one bucket-local `HashMap<u128, EvRaw>`
/// holding a full payload per id. Test oracle only.
#[cfg(test)]
fn oracle_bucket(
    b: usize,
    parts: &[Vec<Vec<EvRaw>>],
    people: &PersonMap,
    bits: u32,
    nb: usize,
) -> Vec<Vec<Ev>> {
    let total: usize = parts.iter().map(|p| p[b].len()).sum();
    let mut kept: HashMap<u128, EvRaw> = HashMap::with_capacity(total);
    for part in parts {
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
    let mut counts = vec![0usize; nb];
    for r in kept.values_mut() {
        if r.flags & VALID == 0 {
            continue;
        }
        r.user = people.person(r.user);
        counts[id::bucket(r.user, bits)] += 1;
    }
    let mut out: Vec<Vec<Ev>> = counts.into_iter().map(Vec::with_capacity).collect();
    for r in kept.values() {
        if r.flags & VALID == 0 {
            continue;
        }
        out[id::bucket(r.user, bits)].push(Ev {
            person: r.user,
            ts: r.ts,
            price: r.price,
            cat: r.cat,
            purchase: r.flags & PURCHASE != 0,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BITS: u32 = 3;
    const NB: usize = 1 << BITS;

    fn pack(s: &str) -> u128 {
        id::pack(s.as_bytes()).unwrap()
    }

    fn person_map(pairs: &[(&str, &str)]) -> PersonMap {
        let pairs: Vec<(u128, u128)> = pairs.iter().map(|&(a, b)| (pack(a), pack(b))).collect();
        super::super::people::from_pairs(pairs.into_iter(), 0)
    }

    fn raw(id: u128, user: u128, ingest_s: i32, flags: u8) -> EvRaw {
        EvRaw {
            id,
            user,
            ingest_s,
            ts: 1_000 + ingest_s as u32,
            price: 250,
            cat: 3,
            flags,
        }
    }

    fn parts_of_bits(nparts: usize, rows: Vec<EvRaw>, bits: u32) -> Vec<Vec<Vec<EvRaw>>> {
        let nb = 1usize << bits;
        let mut parts: Vec<Vec<Vec<EvRaw>>> =
            (0..nparts).map(|_| (0..nb).map(|_| Vec::new()).collect()).collect();
        for (i, r) in rows.into_iter().enumerate() {
            if nparts > 0 {
                parts[i % nparts][id::bucket(r.id, bits)].push(r);
            }
        }
        parts
    }

    fn parts_of(nparts: usize, rows: Vec<EvRaw>) -> Vec<Vec<Vec<EvRaw>>> {
        parts_of_bits(nparts, rows, BITS)
    }

    /// The old nested scatter, gathered the way P3 reads it: per person bucket.
    fn gather_old(out: Vec<Vec<Vec<Ev>>>, nb: usize) -> Vec<Vec<Ev>> {
        (0..nb)
            .map(|pb| out.iter().flat_map(|o| o[pb].iter().copied()).collect())
            .collect()
    }

    /// The flat P2 output, gathered the way P3 reads it (empty slices included).
    fn gather_new(out: &[Flat], nb: usize) -> Vec<Vec<Ev>> {
        (0..nb)
            .map(|pb| out.iter().flat_map(|f| f.bucket(pb).iter().copied()).collect())
            .collect()
    }

    /// Sort every person bucket, so only the multiset is compared.
    fn sort_gathered(mut v: Vec<Vec<Ev>>) -> Vec<Vec<Ev>> {
        for bucket in &mut v {
            bucket.sort_unstable_by_key(|e| (e.person, e.ts, e.price, e.cat, e.purchase));
        }
        v
    }

    fn run_bits(
        parts: &[Vec<Vec<EvRaw>>],
        people: &PersonMap,
        bits: u32,
    ) -> (Vec<Vec<Ev>>, Vec<Vec<Ev>>) {
        let nb = 1usize << bits;
        let new: Vec<Flat> = (0..nb)
            .map(|b| partition_bucket(b, parts, people, bits, nb).expect("offsets fit"))
            .collect();
        let old: Vec<Vec<Vec<Ev>>> = (0..nb)
            .map(|b| oracle_bucket(b, parts, people, bits, nb))
            .collect();
        (
            sort_gathered(gather_new(&new, nb)),
            sort_gathered(gather_old(old, nb)),
        )
    }

    /// Sorted per person bucket, so equality is multiset equality.
    fn run(parts: &[Vec<Vec<EvRaw>>], people: &PersonMap) -> (Vec<Vec<Ev>>, Vec<Vec<Ev>>) {
        run_bits(parts, people, BITS)
    }

    fn xorshift(mut seed: u64) -> impl FnMut() -> u64 {
        move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        }
    }

    #[test]
    fn winners_append_only_on_vacancy_and_overwrite_in_place() {
        let (a, b) = (pack("a"), pack("b"));
        let mut w = Winners::with_capacity(0);
        w.insert(raw(a, 1, 5, VALID));
        w.insert(raw(b, 2, 9, VALID));
        w.insert(raw(a, 1, 4, VALID | PURCHASE)); // strictly smaller: overwrite
        w.insert(raw(a, 1, 4, VALID)); // equal: ignored
        w.insert(raw(a, 1, 7, VALID)); // larger: ignored
        assert_eq!(w.len(), 2);
        assert_eq!(w.rows()[0].ingest_s, 4);
        assert_eq!(w.rows()[0].flags, VALID | PURCHASE);
        assert_eq!(w.rows()[1].id, b);
        // Dense indices: every map value is a live row index, and rows keep
        // first-seen order.
        assert_eq!(w.index[&a], 0);
        assert_eq!(w.index[&b], 1);
    }

    #[test]
    fn randomized_shuffled_cross_chunk_duplicates_match_oracle() {
        let mut next = xorshift(0x243f_6a88_85a3_08d3);
        let people = person_map(&[("u1", "u9"), ("u2", "u9")]);
        let users = ["u0", "u1", "u2", "u3", "u4"];
        for round in 0..300 {
            let nparts = (next() % 5) as usize;
            let nids = 1 + (next() % 40) as usize;
            let mut rows = Vec::new();
            for k in 0..nids {
                let idv = pack(&format!("e{round}-{k}"));
                let copies = 1 + (next() % 4) as usize;
                for _ in 0..copies {
                    let user = pack(users[(next() % users.len() as u64) as usize]);
                    // Duplicate ingest values, so equal-ingest ties are exercised.
                    let ingest = (next() % 6) as i32;
                    let flags = if next() % 5 == 0 {
                        0
                    } else {
                        VALID | if next() % 3 == 0 { PURCHASE } else { 0 }
                    };
                    rows.push(raw(idv, user, ingest, flags));
                }
            }
            // Shuffle, so copies of one id land in different parts and orders.
            for i in (1..rows.len()).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                rows.swap(i, j);
            }
            let parts = parts_of(nparts, rows);
            let (new, old) = run(&parts, &people);
            assert_eq!(new, old, "round {round}");
            if nparts == 0 {
                assert!(new.iter().all(Vec::is_empty));
            }
        }
    }

    #[test]
    fn forced_bucket_collisions_keep_full_ids_apart() {
        // Two different ids that share a P1 bucket: only the full id may dedup.
        let b = 2;
        let first = pack("alpha");
        let second = (0..100_000)
            .map(|i| pack(&format!("alpha{i}")))
            .find(|v| *v != first && id::bucket(*v, b) == id::bucket(first, b))
            .expect("a colliding id");
        assert_ne!(id::bucket(first, b), usize::MAX);
        assert_eq!(id::bucket(first, b), id::bucket(second, b));
        let people = person_map(&[]);
        let rows = vec![
            raw(first, pack("u1"), 9, VALID),
            raw(second, pack("u2"), 1, VALID),
            raw(first, pack("u1"), 2, VALID),
            raw(second, pack("u2"), 8, VALID),
        ];
        let parts = parts_of(2, rows);
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let got: Vec<Ev> = new.iter().flatten().copied().collect();
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|e| e.person == pack("u1") && e.ts == 1_002));
        assert!(got.iter().any(|e| e.person == pack("u2") && e.ts == 1_001));
    }

    #[test]
    fn earlier_invalid_later_valid_is_dropped() {
        let idv = pack("evt");
        let user = pack("u1");
        let people = person_map(&[]);
        // Earlier copy invalid, later copy valid: dedup keeps the invalid one,
        // then validation drops the event (rules.md Events 3, E5).
        let parts = parts_of(
            2,
            vec![raw(idv, user, 1, 0), raw(idv, user, 2, VALID)],
        );
        let (new, old) = run(&parts, &people);
        assert!(new.iter().all(Vec::is_empty));
        assert_eq!(new, old);
        // A later invalid copy does not displace a valid winner.
        let parts = parts_of(
            2,
            vec![raw(idv, user, 1, VALID), raw(idv, user, 2, 0)],
        );
        let (new, _) = run(&parts, &people);
        assert_eq!(new.iter().flatten().count(), 1);
    }

    #[test]
    fn earliest_copy_names_the_user_and_resolves_to_person() {
        let idv = pack("evt");
        let (u2, u9) = (pack("u2"), pack("u9"));
        // u9 is linked to u1, so its person is u1 (byte-wise smallest).
        let people = person_map(&[("u9", "u1")]);
        assert_eq!(people.person(u9), pack("u1"));
        // Earlier copy names u9 -> person u1; the later copy's user is ignored.
        let parts = parts_of(
            2,
            vec![raw(idv, u9, 3, VALID), raw(idv, u2, 5, VALID)],
        );
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let got: Vec<Ev> = new.iter().flatten().copied().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].person, pack("u1"));
        assert_eq!(id::bucket(got[0].person, BITS), id::bucket(pack("u1"), BITS));
        // A smaller ingest on the second copy moves the winner (and its user)
        // over; the earlier copy's user is not resolved at all.
        let parts = parts_of(
            2,
            vec![raw(idv, u9, 5, VALID), raw(idv, u2, 3, VALID)],
        );
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let got: Vec<Ev> = new.iter().flatten().copied().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].person, u2);
    }

    #[test]
    fn equal_ingest_copies_are_interchangeable() {
        let idv = pack("evt");
        let (u1, u2) = (pack("u1"), pack("u2"));
        let people = person_map(&[]);
        let parts = parts_of(
            2,
            vec![raw(idv, u1, 4, VALID), raw(idv, u2, 4, VALID)],
        );
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let got: Vec<Ev> = new.iter().flatten().copied().collect();
        assert_eq!(got.len(), 1);
        // Either copy is allowed; it is exactly one of the two.
        assert!(
            got[0].person == u1 || got[0].person == u2,
            "unexpected winner {:?}",
            got[0]
        );
        assert_eq!(got[0].person, u1, "first-encountered copy wins ties");
    }

    #[test]
    fn empty_buckets_and_empty_parts() {
        let people = person_map(&[]);
        let empty: Vec<Vec<Vec<EvRaw>>> = (0..2).map(|_| (0..NB).map(|_| Vec::new()).collect()).collect();
        let (new, old) = run(&empty, &people);
        assert_eq!(new.len(), NB);
        assert!(new.iter().all(Vec::is_empty));
        assert_eq!(new, old);
        // No parts at all still yields one empty bucket per person bucket.
        let (new, old) = run(&[], &people);
        assert_eq!(new.len(), NB);
        assert!(new.iter().all(Vec::is_empty));
        assert_eq!(new, old);
        // A bucket with rows while its siblings stay empty.
        let parts = parts_of(1, vec![raw(pack("evt"), pack("u1"), 1, VALID)]);
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let occupied = new.iter().filter(|v| !v.is_empty()).count();
        assert_eq!(occupied, 1);
    }

    /// Per person-bucket counts of the old nested scatter for one dedup task.
    fn oracle_counts(parts: &[Vec<Vec<EvRaw>>], people: &PersonMap, b: usize) -> Vec<usize> {
        oracle_bucket(b, parts, people, BITS, NB)
            .iter()
            .map(Vec::len)
            .collect()
    }

    fn shuffled(mut rows: Vec<EvRaw>, next: &mut impl FnMut() -> u64) -> Vec<EvRaw> {
        for i in (1..rows.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            rows.swap(i, j);
        }
        rows
    }

    #[test]
    fn offsets_are_checked_prefix_sums_and_end_at_final_counts() {
        let mut next = xorshift(0x1319_8a2e_0370_7344);
        let people = person_map(&[("u1", "u9"), ("u2", "u9")]);
        let mut rows = Vec::new();
        for k in 0..400 {
            let idv = pack(&format!("offsets-{k}"));
            for c in 0..1 + next() % 3 {
                // Skewed: nine rows in ten name one of the two linked IDs.
                let user = if next() % 10 != 0 {
                    pack(if next() % 2 == 0 { "u1" } else { "u9" })
                } else {
                    pack(&format!("u{}", next() % 6))
                };
                let flags = if c == 0 && next() % 7 == 0 {
                    0
                } else {
                    VALID | if next() % 3 == 0 { PURCHASE } else { 0 }
                };
                rows.push(raw(idv, user, (next() % 5) as i32, flags));
            }
        }
        let parts = parts_of(3, shuffled(rows, &mut next));
        for b in 0..NB {
            let flat = partition_bucket(b, &parts, &people, BITS, NB).expect("offsets fit");
            let counts = oracle_counts(&parts, &people, b);
            let offsets = flat.offsets();
            assert_eq!(offsets.len(), NB + 1);
            assert_eq!(offsets[0], 0);
            assert!(offsets.windows(2).all(|w| w[0] <= w[1]));
            let mut expected = vec![0usize];
            for &c in &counts {
                expected.push(expected.last().unwrap() + c);
            }
            assert_eq!(offsets, expected.as_slice(), "bucket {b}");
            let gathered: usize = (0..NB).map(|pb| flat.bucket(pb).len()).sum();
            assert_eq!(offsets[NB], gathered);
            assert_eq!(offsets[NB], counts.iter().sum::<usize>());
        }
    }

    #[test]
    fn heavily_skewed_people_match_the_nested_scatter() {
        let mut next = xorshift(0xc0ff_ee00_1234_5678);
        // u1, u3, u5 and u7 are one person; only u8 is another.
        let people = person_map(&[("u1", "u7"), ("u3", "u7"), ("u5", "u7")]);
        let mut rows = Vec::new();
        for k in 0..600 {
            let idv = pack(&format!("skew-{k}"));
            for _ in 0..1 + next() % 2 {
                let user = if next() % 20 != 0 {
                    pack(if next() % 2 == 0 { "u1" } else { "u7" })
                } else {
                    pack(["u3", "u5", "u8"][(next() % 3) as usize])
                };
                let flags = if next() % 9 == 0 { 0 } else { VALID };
                rows.push(raw(idv, user, (next() % 6) as i32, flags));
            }
        }
        let parts = parts_of(4, shuffled(rows, &mut next));
        let (new, old) = run(&parts, &people);
        assert_eq!(new, old);
        let lens: Vec<usize> = new.iter().map(Vec::len).collect();
        let occupied = lens.iter().filter(|n| **n > 0).count();
        assert!(occupied <= 2, "people should crowd few buckets: {lens:?}");
        assert!(lens.iter().sum::<usize>() > 0);
        assert!(lens.iter().max().unwrap() > &(lens.iter().sum::<usize>() / 2));
    }

    #[test]
    fn single_bucket_flat_matches_the_nested_scatter() {
        let mut next = xorshift(0x4528_21e6_38d0_1377);
        let people = person_map(&[("u2", "u1"), ("u3", "u1")]);
        let mut rows = Vec::new();
        for k in 0..50 {
            let idv = pack(&format!("solo-{k}"));
            for _ in 0..1 + next() % 3 {
                let user = pack(["u1", "u2", "u3", "u4"][(next() % 4) as usize]);
                let flags = if next() % 4 == 0 {
                    0
                } else {
                    VALID | if next() % 3 == 0 { PURCHASE } else { 0 }
                };
                rows.push(raw(idv, user, (next() % 4) as i32, flags));
            }
        }
        let parts = parts_of_bits(2, shuffled(rows, &mut next), 0);
        let (new, old) = run_bits(&parts, &people, 0);
        assert_eq!(new.len(), 1);
        assert_eq!(new, old);
        // One task, one bucket: the whole output is one contiguous range.
        let flat = partition_bucket(0, &parts, &people, 0, 1).expect("offsets fit");
        assert_eq!(flat.offsets().to_vec(), vec![0, new[0].len()]);
        assert_eq!(flat.bucket(0).len(), new[0].len());
    }
}
