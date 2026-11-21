//! Linked IDs per `rules.md` "Linked IDs".
//!
//! Implemented by T04.

use hashbrown::HashMap;
use std::sync::Arc;

/// Union-find over byte-string IDs. The person's name is the byte-wise smallest
/// ID of its component.
pub struct People {
    /// Canonical storage for every ID seen in a non-empty link. An ID appears
    /// once; the index maps the same allocation back to its node.
    ids: Vec<Arc<[u8]>>,
    /// Canonical root node per ID, finalized during `build`. The root's ID is
    /// the smallest in its component.
    root: Vec<usize>,
    /// Intern table. Lookups borrow `&[u8]`, so `name` allocates nothing.
    index: HashMap<Arc<[u8]>, usize>,
}

impl People {
    pub fn build<'a>(pairs: impl Iterator<Item = (&'a [u8], &'a [u8])>) -> Self {
        let mut people = Self {
            ids: Vec::new(),
            root: Vec::new(),
            index: HashMap::new(),
        };
        for (a, b) in pairs {
            // rules.md "Linked IDs": a line with either field empty is dropped.
            if a.is_empty() || b.is_empty() {
                continue;
            }
            let a = people.intern(a);
            let b = people.intern(b);
            people.union(a, b);
        }
        // Flatten every node to its final canonical root so `name` needs no
        // parent traversal.
        for node in 0..people.root.len() {
            people.root[node] = Self::find(&mut people.root, node);
        }
        people
    }

    /// The person's name: byte-wise smallest ID of its component; an unlinked ID
    /// names itself.
    pub fn name<'a>(&'a self, id: &'a [u8]) -> &'a [u8] {
        match self.index.get(id) {
            Some(&node) => &self.ids[self.root[node]],
            None => id,
        }
    }

    /// Returns the node for `id`, adding an owned copy if it is new.
    fn intern(&mut self, id: &[u8]) -> usize {
        if let Some(&node) = self.index.get(id) {
            return node;
        }
        let owned: Arc<[u8]> = Arc::from(id);
        let node = self.ids.len();
        self.ids.push(Arc::clone(&owned));
        self.root.push(node);
        self.index.insert(owned, node);
        node
    }

    /// Joins two components. The root is the component's byte-wise smallest ID,
    /// which makes the resulting name independent of link order.
    fn union(&mut self, a: usize, b: usize) {
        let a = Self::find(&mut self.root, a);
        let b = Self::find(&mut self.root, b);
        if a == b {
            return;
        }
        let (root, child) = if self.ids[a].as_ref() <= self.ids[b].as_ref() {
            (a, b)
        } else {
            (b, a)
        };
        self.root[child] = root;
    }

    /// Iterative root lookup with path compression; long chains cannot overflow
    /// the stack.
    fn find(parent: &mut [usize], mut node: usize) -> usize {
        let mut root = node;
        while parent[root] != root {
            root = parent[root];
        }
        while node != root {
            let next = parent[node];
            parent[node] = root;
            node = next;
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::People;

    fn build(pairs: &[(&[u8], &[u8])]) -> People {
        People::build(pairs.iter().copied())
    }

    #[test]
    fn unlinked_id_names_itself() {
        let people = build(&[]);
        assert_eq!(people.name(b"u1"), b"u1");
        assert_eq!(people.name(b""), b"");
    }

    #[test]
    fn chains_join_in_either_direction() {
        let people = build(&[(b"c".as_slice(), b"b"), (b"b", b"a")]);
        for id in [b"a".as_slice(), b"b", b"c"] {
            assert_eq!(people.name(id), b"a");
        }

        let people = build(&[(b"a".as_slice(), b"b"), (b"b", b"c")]);
        for id in [b"a".as_slice(), b"b", b"c"] {
            assert_eq!(people.name(id), b"a");
        }
    }

    #[test]
    fn repeated_and_self_links_change_nothing() {
        let people = build(&[
            (b"b".as_slice(), b"a"),
            (b"a", b"b"),
            (b"b", b"b"),
            (b"a", b"a"),
            (b"a", b"b"),
        ]);
        assert_eq!(people.name(b"a"), b"a");
        assert_eq!(people.name(b"b"), b"a");
    }

    #[test]
    fn name_can_be_an_id_seen_only_in_links() {
        let people = build(&[(b"used".as_slice(), b"unused")]);
        assert_eq!(people.name(b"used"), b"unused");
        assert_eq!(people.name(b"unused"), b"unused");
    }

    #[test]
    fn byte_order_is_exact_and_uppercase_sorts_first() {
        let people = build(&[(b"u1".as_slice(), b"U7")]);
        assert_eq!(people.name(b"u1"), b"U7");
        assert_eq!(people.name(b"U7"), b"U7");

        // Spaces are data: "u1 " is neither "u1" nor linked to it.
        let people = build(&[(b"u1".as_slice(), b"u2")]);
        assert_eq!(people.name(b"u1"), b"u1");
        assert_eq!(people.name(b"u1 "), b"u1 ");
        assert_eq!(people.name(b"u"), b"u");
    }

    #[test]
    fn empty_link_sides_are_skipped() {
        // If an empty side were joined it could only be the name if it were the
        // smallest, so check both orders and that no empty ID entered storage.
        let people = build(&[(b"".as_slice(), b"a"), (b"a", b""), (b"", b""), (b"", b"b")]);
        assert_eq!(people.name(b"a"), b"a");
        assert_eq!(people.name(b"b"), b"b");
        assert_eq!(people.name(b""), b"");
        // Every pair had an empty side, so nothing was interned.
        assert_eq!(people.ids.len(), 0);
    }

    #[test]
    fn disconnected_groups_keep_separate_names() {
        let people = build(&[
            (b"b".as_slice(), b"a"),
            (b"d", b"c"),
            (b"e", b"a"),
            (b"f", b"g"),
        ]);
        assert_eq!(people.name(b"a"), b"a");
        assert_eq!(people.name(b"b"), b"a");
        assert_eq!(people.name(b"e"), b"a");
        assert_eq!(people.name(b"c"), b"c");
        assert_eq!(people.name(b"d"), b"c");
        assert_eq!(people.name(b"f"), b"f");
        assert_eq!(people.name(b"g"), b"f");
        assert_eq!(people.name(b"h"), b"h");
    }

    #[test]
    fn canonical_name_is_independent_of_union_order() {
        let pairs: &[(&[u8], &[u8])] = &[
            (b"z".as_slice(), b"m"),
            (b"m", b"a"),
            (b"q", b"z"),
            (b"x", b"y"),
            (b"y", b"b"),
        ];
        let forward = build(pairs);
        let mut reversed_pairs = pairs.to_vec();
        reversed_pairs.reverse();
        let reversed = build(&reversed_pairs);

        for id in [b"a".as_slice(), b"m", b"q", b"z", b"b", b"x", b"y"] {
            assert_eq!(forward.name(id), reversed.name(id));
        }
        assert_eq!(forward.name(b"q"), b"a");
        assert_eq!(forward.name(b"x"), b"b");
    }

    #[test]
    fn four_hundred_link_chain_does_not_overflow() {
        // 401 IDs form a 400-link chain; the last ID is the deepest node.
        let ids: Vec<Vec<u8>> = (0..401).map(|i| format!("n{i:03}").into_bytes()).collect();
        let mut pairs: Vec<(&[u8], &[u8])> = Vec::new();
        // Descending order keeps each union's parent dependency long without
        // compression; find must still be iterative.
        for i in (1..ids.len()).rev() {
            pairs.push((&ids[i], &ids[i - 1]));
        }

        let people = build(&pairs);
        assert_eq!(people.name(&ids[400]), ids[0].as_slice());
        for id in &ids {
            assert_eq!(people.name(id), ids[0].as_slice());
        }
    }
}
