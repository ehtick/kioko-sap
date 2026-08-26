//! The constructors for the typestate `MemFile`.
use crate::errors::file::FileError;

use crate::files::files::mem_files::constants::CONTENT_FIELD;
use crate::files::files::mem_files::full_mem_file::{Async, Backend, Blocking, MemFile};
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};
use yrs::{Doc, Text, Transact, Update, updates::decoder::Decode};

// MARK: - Generic impl

impl<B: Backend> MemFile<B> {
    /// Builds a buffer by seeding a fresh document directly with `contents`.
    ///
    /// This is the backend-agnostic seeder shared by both flavours: the starting text is
    /// supplied by the caller rather than read from a backend, so it cannot fail and never
    /// touches the backend on construction (the backend is only used for later saves). Use
    /// it when you already hold the file's text — for example a frontend that received the
    /// file data over the websocket and wants a buffer for it without a backend round trip.
    ///
    /// Like `from_file` this is an authoritative-origin seeder: it mints brand new CRDT
    /// identities for `contents`, so two replicas that each `from_string` the same text will
    /// not converge if they then edit and merge. A replica joining an existing session must
    /// instead use [`from_state`](MemFile::from_state).
    ///
    /// # Arguments
    /// - `path`: The logical path this buffer writes back to.
    /// - `backend`: The backend typestate the buffer will flush through.
    /// - `contents`: The starting text of the buffer.
    ///
    /// # Returns
    /// The seeded buffer.
    pub fn from_string(path: Path<FilePath>, backend: B, contents: String) -> Self {
        let doc = Doc::new();
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        if !contents.is_empty() {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, &contents);
        }
        Self::new(doc, text, path, backend)
    }

    /// Builds an empty buffer — a thin alias for `from_string(path, backend, "")`.
    ///
    /// This is the starting point for the state-vector sync handshake: an empty document
    /// whose state vector asks the origin for everything it has, ready to be filled by
    /// applying the origin's reply.
    ///
    /// # Arguments
    /// - `path`: The logical path this buffer writes back to.
    /// - `backend`: The backend typestate the buffer will flush through.
    ///
    /// # Returns
    /// An empty seeded buffer.
    pub fn empty(path: Path<FilePath>, backend: B) -> Self {
        Self::from_string(path, backend, "".into())
    }

    /// Builds a buffer for a replica joining an existing collaborative session.
    ///
    /// `state_update` is a `yrs` state update from the authoritative origin. Starting from
    /// it — rather than re-seeding from raw text with `from_file` / `from_string` — means
    /// this replica adopts the origin's CRDT identities, so subsequent concurrent edits on
    /// either side converge. It is backend-agnostic: the decode and apply are synchronous,
    /// so the one constructor serves both the blocking and async flavours.
    ///
    /// # Arguments
    /// - `path`: The logical path this buffer writes back to.
    /// - `state_update`: A v1-encoded state update from the origin replica.
    /// - `backend`: The backend typestate the buffer will flush through.
    ///
    /// # Returns
    /// The joined buffer, or a `FileError::MemFile` if the update cannot be decoded or
    /// applied.
    pub fn from_state(
        path: Path<FilePath>,
        state_update: &[u8],
        backend: B,
    ) -> Result<Self, FileError> {
        let doc = Doc::new();
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        let update = Update::decode_v1(state_update).map_err(|error| FileError::MemFile {
            path: (&path).into(),
            message: error.to_string(),
        })?;
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).map_err(|error| FileError::MemFile {
                path: (&path).into(),
                message: error.to_string(),
            })?;
        }
        Ok(Self::new(doc, text, path, backend))
    }

    /// Sets whether `Drop` makes a final synchronous `save`, returning `self` so it chains
    /// off a constructor.
    ///
    /// Leave it on (the default) on the server, where `Drop` flushing buffered edits to disk
    /// is the safety net. Turn it off on the frontend: there the durable store is
    /// asynchronous (IndexedDB), which `Drop` cannot reach, and a dropped buffer loses
    /// nothing because the relay and the server replica are the source of truth — see the
    /// crate README.
    ///
    /// # Arguments
    /// - `enabled`: Whether a final `save` runs on drop.
    ///
    /// # Returns
    /// The buffer, so the call chains off a constructor.
    ///
    /// ```ignore
    /// let file = MemFile::empty(path, Blocking::new(&store)).flush_on_drop(false);
    /// ```
    pub fn flush_on_drop(mut self, enabled: bool) -> Self {
        self.flush_on_drop = enabled;
        self
    }
}

// MARK: - Blocking impl

impl<'a, S: FileIo> MemFile<Blocking<'a, S>> {
    /// Loads a file from the blocking backend into a new buffer, seeding a fresh document
    /// with its contents.
    ///
    /// Use this for the authoritative origin of a file — typically the server loading from
    /// disk, or the first client to open a file that has never been collaborated on. Seeding
    /// mints brand new CRDT identities for the loaded text, so two replicas that each
    /// `from_file` the same bytes will not converge if they then edit and merge. A replica
    /// joining an existing session must instead start from the origin's state with
    /// [`from_state`](MemFile::from_state).
    ///
    /// # Arguments
    /// - `path`: The logical path to read from and later write back to.
    /// - `backend`: The blocking backend typestate to read the starting text from and later
    ///   flush to.
    ///
    /// # Returns
    /// The seeded buffer, or a `FileError` if the backend holds no file at `path`.
    pub fn from_file(path: Path<FilePath>, backend: Blocking<'a, S>) -> Result<Self, FileError> {
        let contents = backend.store.read_file(&path)?;
        Ok(Self::from_string(path, backend, contents))
    }
}

// MARK: - Async impl

impl<'a, S: AsyncFileIo> MemFile<Async<'a, S>> {
    /// Loads a file from the async backend into a new buffer, seeding a fresh document with
    /// its contents.
    ///
    /// The async counterpart of the blocking `from_file`: the only difference is that the
    /// read from the backend is awaited. It is an authoritative-origin seeder with the same
    /// CRDT-identity caveats — a replica joining an existing session must use
    /// [`from_state`](MemFile::from_state) instead.
    ///
    /// # Arguments
    /// - `path`: The logical path to read from and later write back to.
    /// - `backend`: The async backend typestate to read the starting text from and later
    ///   flush to.
    ///
    /// # Returns
    /// The seeded buffer, or a `FileError` if the backend holds no file at `path`.
    pub async fn from_file(path: Path<FilePath>, backend: Async<'a, S>) -> Result<Self, FileError> {
        let contents = backend.store.read_file(&path).await?;
        Ok(Self::from_string(path, backend, contents))
    }
}

#[cfg(test)]
mod tests {
    // Each test drives a constructor and reads the seeded contents back through the shared
    // `contents()` view. The backend-agnostic constructors (`from_string`, `empty`,
    // `from_state`) are exercised through the blocking backend; `from_file` is covered on
    // both flavours, with the async one driven to completion via `block_on`. The in-memory
    // `BlockingMemIo` / `AsyncMemIo` fakes stand in for real storage.

    use super::*;
    use crate::files::engines::{async_io::mem::AsyncMemIo, blocking_io::mem::BlockingMemIo};
    use futures::executor::block_on;
    use yrs::{ReadTxn, StateVector};

    /// Builds a typed file path from a plain name for use in the tests.
    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    /// Encodes `contents` as a v1 `yrs` state update, matching what an authoritative origin
    /// would hand a joining replica. This is the valid input `from_state` expects.
    fn encoded_update(contents: &str) -> Vec<u8> {
        let doc = Doc::new();
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, contents);
        }
        doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    // MARK: - Backend-agnostic constructors

    #[test]
    fn from_string_seeds_contents() {
        let store = BlockingMemIo::new();
        let mem =
            MemFile::from_string(file("main.cad"), Blocking::new(&store), "hello world".into());

        assert_eq!(mem.contents(), "hello world");
    }

    #[test]
    fn empty_has_no_contents() {
        let store = BlockingMemIo::new();
        let mem = MemFile::empty(file("main.cad"), Blocking::new(&store));

        assert_eq!(mem.contents(), "");
    }

    #[test]
    fn from_state_joins_origin_contents() {
        let store = BlockingMemIo::new();
        let update = encoded_update("shared state");

        let mem = MemFile::from_state(file("main.cad"), &update, Blocking::new(&store)).unwrap();
        assert_eq!(mem.contents(), "shared state");
    }

    #[test]
    fn from_state_rejects_malformed_update() {
        let store = BlockingMemIo::new();
        // Bytes that are not a valid v1 update; decoding must fail rather than panic.
        let result =
            MemFile::from_state(file("main.cad"), &[0xff, 0x00, 0x13, 0x37], Blocking::new(&store));

        assert!(matches!(result, Err(FileError::MemFile { .. })));
    }

    #[test]
    fn flush_on_drop_builder_sets_flag() {
        let store = BlockingMemIo::new();
        let mem = MemFile::empty(file("main.cad"), Blocking::new(&store)).flush_on_drop(false);

        assert!(!mem.flush_on_drop);
    }

    // MARK: - Blocking from_file

    #[test]
    fn blocking_from_file_reads_stored_contents() {
        let store = BlockingMemIo::new();
        store.write_file(&file("main.cad"), "on disk").unwrap();

        // Two `from_file` constructors share the name (blocking + async) with no receiver to
        // disambiguate, so the Self type is named explicitly.
        let mem =
            MemFile::<Blocking<'_, _>>::from_file(file("main.cad"), Blocking::new(&store)).unwrap();
        assert_eq!(mem.contents(), "on disk");
    }

    #[test]
    fn blocking_from_file_missing_file_errors() {
        let store = BlockingMemIo::new();

        assert!(
            MemFile::<Blocking<'_, _>>::from_file(file("missing.cad"), Blocking::new(&store))
                .is_err()
        );
    }

    // MARK: - Async from_file

    #[test]
    fn async_from_file_reads_stored_contents() {
        let store = AsyncMemIo::new();
        block_on(async {
            store.write_file(&file("main.cad"), "on disk").await.unwrap();

            let mem = MemFile::<Async<'_, _>>::from_file(file("main.cad"), Async::new(&store))
                .await
                .unwrap();
            assert_eq!(mem.contents(), "on disk");
        });
    }

    #[test]
    fn async_from_file_missing_file_errors() {
        let store = AsyncMemIo::new();
        block_on(async {
            assert!(
                MemFile::<Async<'_, _>>::from_file(file("missing.cad"), Async::new(&store))
                    .await
                    .is_err()
            );
        });
    }
}
