//! Linked IDs on packed IDs: user ID -> person name (rules.md "Linked IDs").

use hashbrown::HashMap;
use rayon::prelude::*;

use crate::id;

use super::scan;
use super::split::split;

pub struct PersonMap {
    /// Only IDs whose name differs from themselves.
    names: HashMap<u128, u128>,
}

impl PersonMap {
    #[inline]
    pub fn person(&self, user: u128) -> u128 {
        self.names.get(&user).copied().unwrap_or(user)
    }
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let up = parent[parent[x as usize] as usize];
        parent[x as usize] = up;
        x = up;
    }
    x
}

/// `None` when a link side is too long to pack (V1 fallback).
pub fn build(chunks: &[&[u8]]) -> Option<PersonMap> {
    let pairs: Vec<Vec<(u128, u128)>> = chunks
        .par_iter()
        .map(|chunk| {
            let mut out = Vec::new();
            for line in scan::lines(chunk) {
                let Some([Some(a), Some(b)]) = split::<2>(line) else {
                    continue;
                };
                out.push((id::pack(a)?, id::pack(b)?));
            }
            Some(out)
        })
        .collect::<Option<_>>()?;
    let total: usize = pairs.iter().map(Vec::len).sum();
    Some(from_pairs(pairs.iter().flatten().copied(), total))
}

pub fn from_pairs(pairs: impl Iterator<Item = (u128, u128)>, hint: usize) -> PersonMap {
    let mut index: HashMap<u128, u32> = HashMap::with_capacity(hint);
    let mut ids: Vec<u128> = Vec::with_capacity(hint);
    let mut parent: Vec<u32> = Vec::with_capacity(hint);
    let mut slot = |v: u128, ids: &mut Vec<u128>, parent: &mut Vec<u32>| -> u32 {
        *index.entry(v).or_insert_with(|| {
            let i = ids.len() as u32;
            ids.push(v);
            parent.push(i);
            i
        })
    };
    for (a, b) in pairs {
        let ia = slot(a, &mut ids, &mut parent);
        let ib = slot(b, &mut ids, &mut parent);
        let (ra, rb) = (find(&mut parent, ia), find(&mut parent, ib));
        if ra != rb {
            parent[ra as usize] = rb;
        }
    }
    let mut name = vec![u128::MAX; ids.len()];
    for i in 0..ids.len() as u32 {
        let r = find(&mut parent, i) as usize;
        name[r] = name[r].min(ids[i as usize]);
    }
    let mut names = HashMap::new();
    for i in 0..ids.len() as u32 {
        let n = name[find(&mut parent, i) as usize];
        if n != ids[i as usize] {
            names.insert(ids[i as usize], n);
        }
    }
    PersonMap { names }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::People;

    fn p(s: &str) -> u128 {
        id::pack(s.as_bytes()).unwrap()
    }

    #[test]
    fn chains_and_names() {
        let map = from_pairs(
            [("u3", "u9"), ("u9", "u2"), ("U7", "u9"), ("u10", "u10")]
                .iter()
                .map(|&(a, b)| (p(a), p(b))),
            0,
        );
        for u in ["u3", "u9", "u2", "U7"] {
            assert_eq!(map.person(p(u)), p("U7"));
        }
        assert_eq!(map.person(p("u10")), p("u10"));
        assert_eq!(map.person(p("u5")), p("u5"));
    }

    #[test]
    fn long_chain() {
        let ids: Vec<String> = (0..400).map(|i| format!("x{:03}", 399 - i)).collect();
        let map = from_pairs(ids.windows(2).map(|w| (p(&w[0]), p(&w[1]))), 0);
        for id in &ids {
            assert_eq!(map.person(p(id)), p("x000"));
        }
    }

    #[test]
    fn matches_v1_people() {
        let mut seed = 0x9e37_79b9_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..1_000 {
            let n = (next() % 30) as usize;
            let links: Vec<(String, String)> = (0..n)
                .map(|_| (format!("u{}", next() % 25), format!("u{}", next() % 25)))
                .collect();
            let v1 = People::build(links.iter().map(|(a, b)| (a.as_bytes(), b.as_bytes())));
            let v2 = from_pairs(links.iter().map(|(a, b)| (p(a), p(b))), 0);
            for k in 0..30 {
                let u = format!("u{k}");
                assert_eq!(v2.person(p(&u)), p(std::str::from_utf8(v1.name(u.as_bytes())).unwrap()));
            }
        }
    }
}
