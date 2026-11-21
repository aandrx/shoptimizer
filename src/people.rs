//! Linked IDs per `rules.md` "Linked IDs".
//!
//! Implemented by T04.

/// Union-find over byte-string IDs. The person's name is the byte-wise smallest
/// ID of its component.
pub struct People {
    _private: (),
}

impl People {
    pub fn build<'a>(pairs: impl Iterator<Item = (&'a [u8], &'a [u8])>) -> Self {
        let _ = pairs;
        todo!("T04: People::build")
    }

    /// The person's name: byte-wise smallest ID of its component; an unlinked ID
    /// names itself.
    pub fn name<'a>(&'a self, id: &'a [u8]) -> &'a [u8] {
        let _ = id;
        todo!("T04: People::name")
    }
}
