//! rules.md "Linked IDs".

use std::collections::HashMap;

/// Union-find over user IDs.
///
/// Every distinct ID that appears in a link gets an index. After `build`, `name_of[i]` is
/// the index of the byte-wise smallest ID of ID `i`'s component.
pub struct People {
    /// ID bytes by index.
    ids: Vec<Vec<u8>>,
    /// ID bytes to index.
    index: HashMap<Vec<u8>, u32>,
    /// For each index, the index of its component's name.
    name_of: Vec<u32>,
}

/// Root of `x`, found without recursion; compresses the path it walks.
fn find(parent: &mut [u32], x: u32) -> u32 {
    let mut root = x;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    let mut cur = x;
    while parent[cur as usize] != root {
        let next = parent[cur as usize];
        parent[cur as usize] = root;
        cur = next;
    }
    root
}

/// Index of `id`, adding it on first sight.
fn intern(
    index: &mut HashMap<Vec<u8>, u32>,
    ids: &mut Vec<Vec<u8>>,
    parent: &mut Vec<u32>,
    size: &mut Vec<u32>,
    id: &[u8],
) -> u32 {
    if let Some(&i) = index.get(id) {
        return i;
    }
    let i = ids.len() as u32;
    ids.push(id.to_vec());
    index.insert(id.to_vec(), i);
    parent.push(i);
    size.push(1);
    i
}

impl People {
    /// Builds the people from valid pairs (both sides non-empty). Order and repeats do not matter.
    pub fn build<'a>(pairs: impl Iterator<Item = (&'a [u8], &'a [u8])>) -> Self {
        let mut index: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut ids: Vec<Vec<u8>> = Vec::new();
        let mut parent: Vec<u32> = Vec::new();
        let mut size: Vec<u32> = Vec::new();

        // Union by size.
        for (a, b) in pairs {
            let ia = intern(&mut index, &mut ids, &mut parent, &mut size, a);
            let ib = intern(&mut index, &mut ids, &mut parent, &mut size, b);
            let ra = find(&mut parent, ia);
            let rb = find(&mut parent, ib);
            if ra != rb {
                let (big, small) = if size[ra as usize] >= size[rb as usize] { (ra, rb) } else { (rb, ra) };
                parent[small as usize] = big;
                size[big as usize] += size[small as usize];
            }
        }

        // Smallest ID per component root, then each ID points at its component's name.
        let n = ids.len();
        let mut root_of = Vec::with_capacity(n);
        for i in 0..n as u32 {
            root_of.push(find(&mut parent, i));
        }
        let mut smallest: Vec<u32> = vec![u32::MAX; n];
        for i in 0..n {
            let r = root_of[i] as usize;
            if smallest[r] == u32::MAX || ids[i] < ids[smallest[r] as usize] {
                smallest[r] = i as u32;
            }
        }
        let name_of = root_of.iter().map(|&r| smallest[r as usize]).collect();

        People { ids, index, name_of }
    }

    /// The person's name: byte-wise smallest ID of its component; an unlinked ID names itself.
    pub fn name<'a>(&'a self, id: &'a [u8]) -> &'a [u8] {
        match self.index.get(id) {
            Some(&i) => &self.ids[self.name_of[i as usize] as usize],
            None => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people(pairs: &[(&str, &str)]) -> People {
        People::build(pairs.iter().map(|(a, b)| (a.as_bytes(), b.as_bytes())))
    }

    fn name_str<'a>(p: &'a People, id: &'a str) -> &'a str {
        std::str::from_utf8(p.name(id.as_bytes())).unwrap()
    }

    #[test]
    fn chain_of_two_links_names_smallest() {
        let p = people(&[("u3", "u9"), ("u9", "u2")]);
        assert_eq!(name_str(&p, "u3"), "u2");
        assert_eq!(name_str(&p, "u9"), "u2");
        assert_eq!(name_str(&p, "u2"), "u2");
    }

    #[test]
    fn links_work_in_either_direction() {
        let p = people(&[("u9", "u3"), ("u2", "u9")]);
        assert_eq!(name_str(&p, "u3"), "u2");
        assert_eq!(name_str(&p, "u9"), "u2");
        let q = people(&[("u2", "u9"), ("u3", "u9")]);
        assert_eq!(name_str(&q, "u3"), "u2");
    }

    #[test]
    fn long_chain_in_random_order_names_smallest() {
        // 400 links c0000-c0001, ..., c0399-c0400, shuffled, each reversed at random.
        let mut pairs: Vec<(String, String)> = (0..400)
            .map(|i| (format!("c{:04}", i), format!("c{:04}", i + 1)))
            .collect();
        // Deterministic xorshift, so no dependency is needed.
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for i in (1..pairs.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            pairs.swap(i, j);
        }
        for pair in pairs.iter_mut() {
            if next() % 2 == 0 {
                std::mem::swap(&mut pair.0, &mut pair.1);
            }
        }
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let p = people(&refs);
        for i in 0..=400 {
            assert_eq!(name_str(&p, &format!("c{:04}", i)), "c0000", "id c{:04}", i);
        }
        assert_eq!(name_str(&p, "c0401"), "c0401");
    }

    #[test]
    fn self_link_names_itself() {
        let p = people(&[("u6", "u6")]);
        assert_eq!(name_str(&p, "u6"), "u6");
    }

    #[test]
    fn repeated_link_either_way_changes_nothing() {
        let once = people(&[("u5", "u2")]);
        let repeated = people(&[("u5", "u2"), ("u2", "u5"), ("u5", "u2"), ("u6", "u6")]);
        assert_eq!(name_str(&once, "u5"), "u2");
        assert_eq!(name_str(&repeated, "u5"), "u2");
        assert_eq!(name_str(&repeated, "u2"), "u2");
        assert_eq!(name_str(&repeated, "u6"), "u6");
    }

    #[test]
    fn link_only_id_can_be_the_name() {
        // L2: "U7" < "u9" byte-wise, and U7 is never in an event.
        let p = people(&[("u3", "u9"), ("u9", "u2"), ("U7", "u9"), ("u10", "u10")]);
        assert_eq!(name_str(&p, "u3"), "U7");
        assert_eq!(name_str(&p, "u2"), "U7");
        assert_eq!(name_str(&p, "u9"), "U7");
        assert_eq!(name_str(&p, "U7"), "U7");
        assert_eq!(name_str(&p, "u10"), "u10");
    }

    #[test]
    fn unseen_id_names_itself() {
        let p = people(&[("u1", "u2")]);
        assert_eq!(name_str(&p, "zzz"), "zzz");
        let empty = People::build(std::iter::empty());
        assert_eq!(name_str(&empty, "zzz"), "zzz");
    }

    #[test]
    fn case_matters() {
        let p = people(&[("u1", "x")]);
        assert_eq!(name_str(&p, "U1"), "U1");
        assert_eq!(name_str(&p, "u1"), "u1");
        let q = people(&[("u1", "U1")]);
        assert_eq!(name_str(&q, "u1"), "U1");
        assert_eq!(name_str(&q, "U1"), "U1");
    }

    #[test]
    fn leading_space_is_a_different_id() {
        // " u1" < "u1" byte-wise, but it is not the same ID as "u1".
        let p = people(&[(" u1", "u2")]);
        assert_eq!(name_str(&p, " u1"), " u1");
        assert_eq!(name_str(&p, "u2"), " u1");
        assert_eq!(name_str(&p, "u1"), "u1");
        let q = people(&[("u1", "u2")]);
        assert_eq!(name_str(&q, " u1"), " u1");
    }

    #[test]
    fn trailing_space_is_a_different_id() {
        let p = people(&[("u1 ", "u2")]);
        assert_eq!(name_str(&p, "u1"), "u1");
        assert_eq!(name_str(&p, "u1 "), "u1 ");
    }
}
