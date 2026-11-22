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

/// Dedup one event bucket across every worker's part, then re-partition the
/// valid winners by person bucket.
///
/// Validation runs only on the winner (rules.md Events 3, E5), and the winner's
/// user is resolved to a person exactly once (E4) before the capacity count, so
/// the fill pass needs no second lookup.
pub fn partition_bucket(
    b: usize,
    parts: &[Vec<Vec<EvRaw>>],
    people: &PersonMap,
    bits: u32,
    nb: usize,
) -> Vec<Vec<Ev>> {
    let total: usize = parts.iter().map(|p| p[b].len()).sum();
    let mut winners = Winners::with_capacity(total);
    for part in parts {
        for r in &part[b] {
            winners.insert(*r);
        }
    }
    // Count VALID survivors per destination person bucket, so each output Vec
    // gets its exact capacity (S3-5).
    let mut counts = vec![0usize; nb];
    for r in winners.rows_mut() {
        if r.flags & VALID == 0 {
            continue;
        }
        r.user = people.person(r.user);
        counts[id::bucket(r.user, bits)] += 1;
    }
    let mut out: Vec<Vec<Ev>> = counts.into_iter().map(Vec::with_capacity).collect();
    for r in winners.rows() {
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

    fn parts_of(nparts: usize, rows: Vec<EvRaw>) -> Vec<Vec<Vec<EvRaw>>> {
        let mut parts: Vec<Vec<Vec<EvRaw>>> =
            (0..nparts).map(|_| (0..NB).map(|_| Vec::new()).collect()).collect();
        for (i, r) in rows.into_iter().enumerate() {
            if nparts > 0 {
                parts[i % nparts][id::bucket(r.id, BITS)].push(r);
            }
        }
        parts
    }

    fn sorted(out: Vec<Vec<Vec<Ev>>>) -> Vec<Vec<Vec<Ev>>> {
        out.into_iter()
            .map(|bucket| {
                bucket
                    .into_iter()
                    .map(|mut v| {
                        v.sort_unstable_by_key(|e| (e.person, e.ts, e.price, e.cat, e.purchase));
                        v
                    })
                    .collect()
            })
            .collect()
    }

    fn run(
        parts: &[Vec<Vec<EvRaw>>],
        people: &PersonMap,
    ) -> (Vec<Vec<Vec<Ev>>>, Vec<Vec<Vec<Ev>>>) {
        let new = (0..NB)
            .map(|b| partition_bucket(b, parts, people, BITS, NB))
            .collect();
        let old = (0..NB)
            .map(|b| oracle_bucket(b, parts, people, BITS, NB))
            .collect();
        (new, old)
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
            assert_eq!(sorted(new.clone()), sorted(old), "round {round}");
            if nparts == 0 {
                assert!(new.iter().flatten().all(Vec::is_empty));
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
        assert_eq!(sorted(new.clone()), sorted(old));
        let got: Vec<Ev> = new.iter().flatten().flatten().copied().collect();
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
        assert!(new.iter().flatten().all(Vec::is_empty));
        assert_eq!(sorted(new.clone()), sorted(old));
        // A later invalid copy does not displace a valid winner.
        let parts = parts_of(
            2,
            vec![raw(idv, user, 1, VALID), raw(idv, user, 2, 0)],
        );
        let (new, _) = run(&parts, &people);
        assert_eq!(new.iter().flatten().flatten().count(), 1);
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
        assert_eq!(sorted(new.clone()), sorted(old));
        let got: Vec<Ev> = new.iter().flatten().flatten().copied().collect();
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
        assert_eq!(sorted(new.clone()), sorted(old));
        let got: Vec<Ev> = new.iter().flatten().flatten().copied().collect();
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
        assert_eq!(sorted(new.clone()), sorted(old));
        let got: Vec<Ev> = new.iter().flatten().flatten().copied().collect();
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
        assert!(new.iter().flatten().all(Vec::is_empty));
        assert_eq!(sorted(new), sorted(old));
        // No parts at all still yields one empty Vec per bucket.
        let (new, old) = run(&[], &people);
        assert_eq!(new.len(), NB);
        assert!(new.iter().flatten().all(Vec::is_empty));
        assert_eq!(sorted(new), sorted(old));
        // A bucket with rows while its siblings stay empty.
        let parts = parts_of(1, vec![raw(pack("evt"), pack("u1"), 1, VALID)]);
        let (new, old) = run(&parts, &people);
        assert_eq!(sorted(new.clone()), sorted(old));
        let occupied = new.iter().flatten().filter(|v| !v.is_empty()).count();
        assert_eq!(occupied, 1);
    }
}
