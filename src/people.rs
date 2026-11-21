//! rules.md "Linked IDs".

/// Union-find over user IDs.
pub struct People {}

impl People {
    pub fn build<'a>(pairs: impl Iterator<Item = (&'a [u8], &'a [u8])>) -> Self {
        let _ = pairs;
        todo!()
    }

    /// The person's name: byte-wise smallest ID of its component; an unlinked ID names itself.
    pub fn name<'a>(&'a self, id: &'a [u8]) -> &'a [u8] {
        let _ = id;
        todo!()
    }
}
