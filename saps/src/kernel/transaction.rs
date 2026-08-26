//! The data for a batch of file edits sent to and from the git file actor.
//!
//! A transaction is an ordered list of edits aimed at one file. Each edit is an
//! [`Operation`], and each operation is one of three kinds: insert a run of
//! characters, delete a run of characters, or swap a run for a different run.
//! This module holds only the data — where each edit lands and the characters it
//! carries. Applying a transaction to an in-memory buffer lives next to that
//! buffer in the `projects-core` crate, which builds on top of these types.
//!
//! A transaction carries a `Vec` of operations rather than a single edit so a
//! whole multi-edit change (for example a find and replace touching several
//! places) can travel as one unit. Today the frontend sends one operation per
//! transaction; the list leaves room to pack more into a single message later on
//! if network throughput ever becomes the bottleneck.
//!
//! Each kind of edit needs different fields, so rather than one struct with
//! fields that go unused per kind, every kind is its own struct ([`InsertSlice`],
//! [`DeleteSlice`], [`SwapSlice`]) and [`Operation`] is the enum that selects
//! between them. The types derive `Serialize`/`Deserialize` so a transaction can
//! be carried in an actor message and re-broadcast onwards. The JS facing surface
//! — the per-operation contracts the frontend builds and sends — lives in the
//! contract-serialization layer, not here.

use serde::{Deserialize, Serialize};

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

/// A line and column position within a file.
#[cfg_attr(feature = "wasm", wasm_bindgen)]
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorIndex {
    pub col: usize,
    pub line: usize,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl CursorIndex {
    /// Builds a position from a line and column.
    ///
    /// # Arguments
    /// - `line`: The zero based line of the position
    /// - `col`: The zero based column of the position
    #[wasm_bindgen(constructor)]
    pub fn new(line: usize, col: usize) -> CursorIndex {
        CursorIndex { col, line }
    }
}

/// Inserts a run of characters into a file.
///
/// The characters in `content` are inserted starting at `position`, pushing the
/// text that was already there to the right. An empty `content` is a no-op.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InsertSlice {
    /// The line and column the first inserted character lands at.
    pub position: CursorIndex,
    /// The characters to insert, in order.
    pub content: String,
}

/// Deletes a run of characters from a file.
///
/// `length` characters are removed starting at `position`, counting forward
/// through the buffer and across line breaks if the run reaches them. A `length`
/// of zero is a no-op.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DeleteSlice {
    /// The line and column the first removed character sits at.
    pub position: CursorIndex,
    /// The number of characters to remove, counting forward from `position`.
    pub length: usize,
}

/// Replaces a run of characters with a different run, in place.
///
/// The `length` characters starting at `position` are removed and `content` is
/// inserted in their place, both anchored at the same `position`. The removed run
/// and the replacement run need not be the same length, so a swap can grow or
/// shrink the file. It is the in-place equivalent of a [`DeleteSlice`] followed by
/// an [`InsertSlice`] at the same spot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SwapSlice {
    /// The line and column the replaced run starts at, and where the replacement
    /// run is inserted.
    pub position: CursorIndex,
    /// The number of characters to remove, counting forward from `position`.
    pub length: usize,
    /// The characters to insert in place of the removed run, in order.
    pub content: String,
}

/// A single edit within a transaction.
///
/// Each variant carries its own struct because the three kinds of edit need
/// different fields: an insert carries the characters to add, a delete carries
/// only how many characters to drop, and a swap carries both.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Insert a run of characters. See [`InsertSlice`].
    Insert(InsertSlice),
    /// Delete a run of characters. See [`DeleteSlice`].
    Delete(DeleteSlice),
    /// Replace a run of characters with a different run. See [`SwapSlice`].
    Swap(SwapSlice),
}

/// An ordered list of edits for one file.
///
/// The operations are applied in list order by whatever consumes the
/// transaction, each one resolved against the buffer as it stands once the
/// earlier operations have landed. So an operation's position must account for
/// the inserts, deletes, and swaps of the operations before it in the same
/// transaction.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    /// The operations to apply, in order.
    operations: Vec<Operation>,
}

impl Transaction {
    /// Builds a transaction from its operations.
    ///
    /// # Arguments
    /// - `operations`: The edits to apply, in the order they should land.
    pub fn new(operations: Vec<Operation>) -> Self {
        Self { operations }
    }

    /// The operations in list order.
    ///
    /// Consumers iterate these to apply the transaction to a buffer.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(line: usize, col: usize) -> CursorIndex {
        CursorIndex { line, col }
    }

    /// A transaction holding one of each operation kind survives a JSON round
    /// trip unchanged, so the tagged data of every variant serializes and
    /// deserializes back to the same value.
    #[test]
    fn transaction_survives_json_round_trip() {
        let transaction = Transaction::new(vec![
            Operation::Insert(InsertSlice { position: cursor(0, 0), content: "hello".to_string() }),
            Operation::Delete(DeleteSlice { position: cursor(0, 5), length: 3 }),
            Operation::Swap(SwapSlice {
                position: cursor(1, 2),
                length: 4,
                content: "world".to_string(),
            }),
        ]);

        let json = serde_json::to_string(&transaction).expect("serialize transaction");
        let decoded: Transaction = serde_json::from_str(&json).expect("deserialize transaction");

        assert_eq!(transaction, decoded);
    }

    /// `operations` hands back the edits in the order they were built, so
    /// consumers apply them in that same order.
    #[test]
    fn operations_keep_their_order() {
        let transaction = Transaction::new(vec![
            Operation::Insert(InsertSlice { position: cursor(0, 0), content: "a".to_string() }),
            Operation::Delete(DeleteSlice { position: cursor(0, 1), length: 1 }),
        ]);

        let operations = transaction.operations();

        assert_eq!(operations.len(), 2);
        assert_eq!(
            operations[0],
            Operation::Insert(InsertSlice { position: cursor(0, 0), content: "a".to_string() })
        );
        assert_eq!(
            operations[1],
            Operation::Delete(DeleteSlice { position: cursor(0, 1), length: 1 })
        );
    }

    /// An empty transaction is valid and carries no operations, the natural
    /// replacement for the old all-`Null` no-op batch.
    #[test]
    fn empty_transaction_has_no_operations() {
        let transaction = Transaction::new(vec![]);
        assert!(transaction.operations().is_empty());
    }
}
