//! One `MemFile` type whose write path is chosen by a backend typestate.
//!
//! Async and blocking `save` genuinely cannot be the same method — one returns a future and
//! the other a value — so the type parameter is not a bare marker but a *backend wrapper*
//! that holds the store: [`Blocking`] wraps a [`FileIo`], [`Async`] wraps an [`AsyncFileIo`].
//! All the IO-free `yrs` transaction logic lives in one `impl<B: Backend>` block written
//! once; only `save` (and anything that triggers it) is written per backend, on the distinct
//! types `MemFile<Blocking<..>>` and `MemFile<Async<..>>`, so both can reuse the name `save`
//! without the two impls overlapping.

use yrs::{Doc, ReadTxn, StateVector, TextRef, Transact};

use crate::files::{
    io::{async_file::AsyncFileIo, file::FileIo},
    paths::{FilePath, Path},
};
use crate::errors::file::FileError;

/// The typestate that selects a buffer's write path.
///
/// Implemented only by [`Blocking`] and [`Async`]. Its main job is to make
/// `MemFile<Blocking<..>>` and `MemFile<Async<..>>` distinct types so each can carry its own
/// `save`. It also carries the one flush behaviour that [`Drop`] needs but cannot specialise
/// per typestate — see [`drop_flush`](Backend::drop_flush).
pub trait Backend {
    /// Flushes `contents` to this backend's store from a synchronous [`Drop`].
    ///
    /// This is the one piece of the drop-flush that differs by backend, and a `Drop` impl
    /// cannot be specialised per typestate, so it is dispatched through the trait instead: the
    /// blocking backend writes synchronously, while the async backend cannot (a `Drop` cannot
    /// await) and so does nothing — the frontend leaves the store to the relay and the server
    /// replica. Errors are swallowed here, since `Drop` cannot surface them.
    ///
    /// # Arguments
    /// - `path`: The logical path to write the contents back to.
    /// - `contents`: The buffer's current text to flush.
    fn drop_flush(&self, path: &Path<FilePath>, contents: String);
}

/// Blocking backend typestate, wrapping a synchronous [`FileIo`] store.
pub struct Blocking<'a, S: FileIo> {
    pub(in crate::files::files::mem_files) store: &'a S,
}

/// Async backend typestate, wrapping an asynchronous [`AsyncFileIo`] store.
pub struct Async<'a, S: AsyncFileIo> {
    pub(in crate::files::files::mem_files) store: &'a S,
}

impl<'a, S: FileIo> Blocking<'a, S> {
    /// Wraps a blocking store as a backend typestate.
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }
}

impl<'a, S: AsyncFileIo> Async<'a, S> {
    /// Wraps an async store as a backend typestate.
    pub fn new(store: &'a S) -> Self {
        Self { store }
    }
}

impl<'a, S: FileIo> Backend for Blocking<'a, S> {
    /// Writes the contents straight through the synchronous [`FileIo::write_file`]. Any error
    /// is swallowed, since this only runs from `Drop`.
    fn drop_flush(&self, path: &Path<FilePath>, contents: String) {
        let _ = self.store.write_file(path, contents);
    }
}

impl<'a, S: AsyncFileIo> Backend for Async<'a, S> {
    /// A no-op: the async store's write is awaited and a `Drop` cannot await, so there is
    /// nothing to do. The frontend relies on the relay and the server replica as the source
    /// of truth instead of a drop-time flush.
    fn drop_flush(&self, _path: &Path<FilePath>, _contents: String) {}
}

/// An in-memory, editable view of a single file, backed by a `yrs` document and flushed to
/// durable storage through whichever backend typestate `B` selects.
pub struct MemFile<B: Backend> {
    /// The collaborative document holding the file's text plus the CRDT metadata needed to
    /// merge concurrent edits.
    pub(in crate::files::files::mem_files) doc: Doc,
    /// A handle to the document's root text field.
    pub(in crate::files::files::mem_files) text: TextRef,
    /// The logical path this buffer loads from and writes back to.
    pub(in crate::files::files::mem_files) path: Path<FilePath>,
    /// Number of single character edits applied since the last save.
    pub(in crate::files::files::mem_files) ops_since_save: usize,
    /// Whether `Drop` should make a final synchronous `save`.
    pub flush_on_drop: bool,
    /// The backend typestate, holding the store `save` writes through.
    pub(in crate::files::files::mem_files) backend: B,
}

// MARK: - Shared logic (IO-free, written once for every backend)

impl<B: Backend> MemFile<B> {
    /// Builds a buffer from an already-seeded document and a backend typestate.
    pub fn new(doc: Doc, text: TextRef, path: Path<FilePath>, backend: B) -> Self {
        Self { doc, text, path, ops_since_save: 0, flush_on_drop: true, backend }
    }

    /// The document's state as a v1 update, for handing to a joining replica. Also IO-free
    /// and shared.
    pub fn state(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }
}

// MARK: - Blocking-only write path

impl<'a, S: FileIo> MemFile<Blocking<'a, S>> {
    /// Flushes the current contents to the blocking store.
    ///
    /// This is the coloured half: it calls the synchronous [`FileIo::write_file`]. Anything
    /// that triggers a save (for example a debounced auto-save on `record_single_edit`) also
    /// lives in this impl for the same reason.
    pub fn save(&mut self) -> Result<(), FileError> {
        let contents = self.contents();
        self.backend.store.write_file(&self.path, contents)?;
        self.ops_since_save = 0;
        Ok(())
    }
}

// MARK: - Async-only write path

impl<'a, S: AsyncFileIo> MemFile<Async<'a, S>> {
    /// Flushes the current contents to the async store.
    ///
    /// The async twin of the blocking `save`: identical but for the awaited
    /// [`AsyncFileIo::write_file`].
    pub async fn save(&mut self) -> Result<(), FileError> {
        let contents = self.contents();
        self.backend.store.write_file(&self.path, contents).await?;
        self.ops_since_save = 0;
        Ok(())
    }
}

// MARK: - Drop flush

/// Flushes any buffered edits to the backend's store when the buffer is dropped.
///
/// Single character edits sit in the buffer until [`SAVE_THRESHOLD`](crate::files::files::mem_files::constants::SAVE_THRESHOLD)
/// of them build up (see the transaction write path), so a buffer that is dropped mid-run
/// would otherwise lose whatever had not yet crossed the threshold. This is the server side
/// safety net named in [`flush_on_drop`](MemFile::flush_on_drop): a final synchronous flush on
/// the way out.
///
/// `Drop` cannot be specialised per typestate, so the backend-specific part is dispatched
/// through [`Backend::drop_flush`]: the blocking backend writes synchronously, while the async
/// backend does nothing (a `Drop` cannot await the async store). The frontend turns
/// `flush_on_drop` off and relies on the relay and the server replica as the source of truth
/// instead. `Drop` cannot surface an error, so a failing flush is swallowed; reach for an
/// explicit `save` (or a guard level snapshot) when you need to observe failures.
impl<B: Backend> Drop for MemFile<B> {
    fn drop(&mut self) {
        if self.flush_on_drop {
            let contents = self.contents();
            self.backend.drop_flush(&self.path, contents);
        }
    }
}

#[cfg(test)]
mod tests {
    // Proves the single `MemFile` type dispatches `save()` to the sync or async trait purely
    // by which backend typestate it was built with. The shared `contents()` is written once.

    use super::*;
    use crate::files::engines::{async_io::mem::AsyncMemIo, blocking_io::mem::BlockingMemIo};
    use crate::files::files::mem_files::constants::CONTENT_FIELD;
    use futures::executor::block_on;
    use crate::kernel::transaction::CursorIndex;
    use yrs::{Text, Transact};

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    /// Seeds a fresh document with `contents` under the root text field.
    fn seeded(contents: &str) -> (Doc, TextRef) {
        let doc = Doc::new();
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, contents);
        }
        (doc, text)
    }

    #[test]
    fn blocking_save_writes_through_sync_trait() {
        let store = BlockingMemIo::new();
        let (doc, text) = seeded("hello");
        let mut mem = MemFile::new(doc, text, file("main.cad"), Blocking::new(&store));

        mem.save().unwrap();
        assert_eq!(store.read_file(&file("main.cad")).unwrap(), "hello");
    }

    #[test]
    fn async_save_writes_through_async_trait() {
        let store = AsyncMemIo::new();
        let (doc, text) = seeded("hello");
        let mut mem = MemFile::new(doc, text, file("main.cad"), Async::new(&store));

        block_on(async {
            // Same method name, dispatched to the async trait by the backend typestate.
            mem.save().await.unwrap();
            assert_eq!(store.read_file(&file("main.cad")).await.unwrap(), "hello");
        });
    }

    /// A buffered single character edit that never crossed the save threshold is flushed to
    /// the store when the blocking buffer is dropped.
    #[test]
    fn drop_flushes_buffered_edits_when_enabled() {
        let store = BlockingMemIo::new();
        store.write_file(&file("main.cad"), "abc").unwrap();
        {
            let mut mem =
                MemFile::<Blocking<'_, _>>::from_file(file("main.cad"), Blocking::new(&store))
                    .unwrap();
            // A single edit stays buffered (below the save threshold), so the store is
            // untouched while the buffer is alive...
            mem.insert_char(CursorIndex { line: 0, col: 0 }, 'X').unwrap();
            assert_eq!(store.read_file(&file("main.cad")).unwrap(), "abc");
        } // ...until the buffer is dropped, whose flush writes the edit through.
        assert_eq!(store.read_file(&file("main.cad")).unwrap(), "Xabc");
    }

    /// With `flush_on_drop` turned off, dropping the buffer loses the buffered edit rather
    /// than writing it back — the frontend's behaviour where the store is not the source of
    /// truth.
    #[test]
    fn drop_does_not_flush_when_disabled() {
        let store = BlockingMemIo::new();
        store.write_file(&file("main.cad"), "abc").unwrap();
        {
            let mut mem =
                MemFile::<Blocking<'_, _>>::from_file(file("main.cad"), Blocking::new(&store))
                    .unwrap()
                    .flush_on_drop(false);
            mem.insert_char(CursorIndex { line: 0, col: 0 }, 'X').unwrap();
        }
        // The flush on drop was off, so the buffered edit never reached the store.
        assert_eq!(store.read_file(&file("main.cad")).unwrap(), "abc");
    }
}
