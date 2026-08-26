//! The state functions for the typestate `MemFile`.
use crate::errors::file::FileError;

use crate::files::files::mem_files::full_mem_file::{Backend, MemFile};
use yrs::{
    GetString, ReadTxn, StateVector, Transact,
    updates::{decoder::Decode, encoder::Encode},
};

// MARK: - Generic impl

impl<B: Backend> MemFile<B> {
    /// The current text of the buffer, including any unsaved edits.
    ///
    /// This is the materialised view the LSP / compiler reads and the exact bytes
    /// `save` writes to the backend. It is also what makes line and column
    /// addressing work: the offset helpers resolve positions against this string.
    pub fn contents(&self) -> String {
        self.text.get_string(&self.doc.transact())
    }

    /// Encodes the whole document as a `yrs` state update.
    ///
    /// This is what a *joining* replica is seeded with via [`MemFile::from_state`],
    /// and what you persist as a durable CRDT snapshot so a reloaded session can
    /// rebuild the document (not just the flat text) and keep merging.
    pub fn encode_state(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    /// Encodes this replica's state vector — a compact summary of which edits it
    /// already has.
    ///
    /// A joining replica sends this to the origin, which replies with
    /// [`MemFile::encode_diff`] carrying only the edits this replica is missing,
    /// so catch-up does not replay the whole history.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Encodes only the edits missing from a peer with the given `state_vector`.
    ///
    /// The other half of the joining handshake: given a peer's
    /// [`MemFile::state_vector`], produce the minimal update that brings it up to
    /// date.
    pub fn encode_diff(&self, state_vector: &[u8]) -> Result<Vec<u8>, FileError> {
        let remote = StateVector::decode_v1(state_vector).map_err(|error| FileError::MemFile {
            path: self.path.relative_string(),
            message: error.to_string(),
        })?;
        Ok(self.doc.transact().encode_diff_v1(&remote))
    }
}
