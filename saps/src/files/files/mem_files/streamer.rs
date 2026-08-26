//! Streams the CRDT state of every resident buffer in a [`MemFileGuard`], one file at a time.
//!
//! When a replica joins an editing session it needs every open file's document, not its flat
//! text: the `yrs` state update from the authoritative origin, so the joiner adopts the
//! origin's CRDT identities and can keep merging edits (see
//! [`MemFile::from_state`](crate::files::files::mem_files::full_mem_file::MemFile)). The streamer
//! walks the guard's resident buffers and yields each one's `(relative path, state bytes)`,
//! plus a count up front so the consumer knows how many files to expect without a terminator.
//!
//! It is deliberately transport agnostic: it produces a sequence, and the caller decides how
//! to move it — over a websocket, into a channel, or into a collection. It reads the buffers
//! the guard already holds resident (the source of truth, including edits not yet flushed to
//! the store), so callers make the project resident first with
//! [`MemFileGuard::load_all`](super::guard::MemFileGuard::load_all); a binary asset the guard
//! skipped is simply absent from the stream and served from the store on demand instead.

use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::file::FileIo;

/// Streams the resident buffers of a [`MemFileGuard`] as `(relative path, CRDT state)` pairs.
///
/// Borrows the guard for the streamer's lifetime `'g` and reads its buffers immutably, so a
/// stream never mutates the session it is reading. The store lifetime `'a` and its type `S`
/// are carried through only because the guard is generic over them; the streamer itself only
/// ever reads.
pub struct MemFileStreamer<'g, 'a, S: FileIo> {
    /// The guard whose resident buffers are streamed.
    guard: &'g MemFileGuard<'a, S>,
}

impl<'g, 'a, S: FileIo> MemFileStreamer<'g, 'a, S> {
    /// Builds a streamer over `guard`.
    ///
    /// # Arguments
    /// - `guard`: The guard whose resident buffers will be streamed.
    ///
    /// # Returns
    /// A streamer ready to report its [`count`](Self::count) and yield its
    /// [`states`](Self::states).
    pub fn new(guard: &'g MemFileGuard<'a, S>) -> Self {
        Self { guard }
    }

    /// The number of files that will be streamed.
    ///
    /// Send this to a consumer before the files so it knows how many pairs to expect without a
    /// sentinel terminator. It is the count of buffers the guard currently holds resident, so
    /// it excludes any binary asset the guard skipped on load.
    ///
    /// # Returns
    /// The number of resident buffers.
    pub fn count(&self) -> usize {
        self.guard.file_map.len()
    }

    /// Whether there is nothing to stream.
    ///
    /// # Returns
    /// `true` when no buffer is resident, otherwise `false`.
    pub fn is_empty(&self) -> bool {
        self.guard.file_map.is_empty()
    }

    /// Yields each resident buffer as a `(relative path, CRDT state bytes)` pair.
    ///
    /// The path is relative to the guard's store root — the same key the frontend addresses a
    /// file by — and the bytes are the buffer's whole-document `yrs` state (from
    /// [`MemFile::encode_state`](crate::files::files::mem_files::full_mem_file::MemFile)), which a
    /// joining replica seeds a buffer from. The bytes come from the resident buffer, so they
    /// include edits not yet flushed to the store. Iteration order follows the underlying map
    /// and so is unspecified; the consumer keys on the path rather than position.
    ///
    /// # Returns
    /// An iterator of `(relative path, state bytes)`, one per resident buffer.
    pub fn states(&self) -> impl Iterator<Item = (String, Vec<u8>)> + '_ {
        self.guard.file_map.values().map(|file| (file.path.relative_string(), file.encode_state()))
    }
}

// MARK: - Tests

#[cfg(test)]
mod tests {
    // Each test seeds a store, makes the guard resident over it, streams, and decodes each
    // state blob back to its text through `from_state` — proving the bytes are the CRDT state a
    // joining replica would seed from, not just opaque data. Decoding uses a throwaway store
    // and turns flush-on-drop off so the decode never writes anything back.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;
    use crate::files::files::mem_files::full_mem_file::{Blocking, MemFile};
    use crate::files::files::mem_files::guard::MemFileGuard;
    use crate::files::paths::{FilePath, Path};
    use std::collections::HashMap;

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    /// Materialises the text a streamed state blob seeds a buffer with — exactly what a joining
    /// replica does with the same bytes.
    fn decode(state: &[u8]) -> String {
        let store = BlockingMemIo::new();
        MemFile::from_state(file("seed.txt"), state, Blocking::new(&store))
            .expect("decode state")
            .flush_on_drop(false)
            .contents()
    }

    /// Builds a store seeded with the given `(name, contents)` files.
    fn seeded_store(files: &[(&str, &str)]) -> BlockingMemIo {
        let store = BlockingMemIo::new();
        for (name, contents) in files {
            store.write_file(&file(name), *contents).unwrap();
        }
        store
    }

    #[test]
    fn count_reports_resident_buffers() {
        let store = seeded_store(&[("a.txt", "alpha"), ("b.txt", "beta")]);
        let mut guard = MemFileGuard::new(&store);
        guard.load_all(&[file("a.txt"), file("b.txt")]);

        let streamer = MemFileStreamer::new(&guard);

        assert_eq!(streamer.count(), 2);
        assert!(!streamer.is_empty());
    }

    #[test]
    fn states_yield_each_files_crdt_state() {
        let store = seeded_store(&[("a.txt", "alpha"), ("b.txt", "beta")]);
        let mut guard = MemFileGuard::new(&store);
        guard.load_all(&[file("a.txt"), file("b.txt")]);

        let streamer = MemFileStreamer::new(&guard);
        let streamed: HashMap<String, String> =
            streamer.states().map(|(path, state)| (path, decode(&state))).collect();

        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed.get("a.txt").map(String::as_str), Some("alpha"));
        assert_eq!(streamed.get("b.txt").map(String::as_str), Some("beta"));
    }

    #[test]
    fn states_include_unflushed_edits() {
        let store = seeded_store(&[("a.txt", "alpha")]);
        let mut guard = MemFileGuard::new(&store);
        guard.load_all(&[file("a.txt")]);

        // A single char edit stays buffered (below the save threshold), so the store still
        // holds "alpha"; the stream must reflect the resident buffer, not the store.
        guard
            .get_file(&file("a.txt"))
            .expect("get file")
            .insert_char(crate::kernel::transaction::CursorIndex { line: 0, col: 0 }, 'X')
            .unwrap();
        assert_eq!(store.read_file(&file("a.txt")).unwrap(), "alpha");

        let streamer = MemFileStreamer::new(&guard);
        let (_, state) = streamer.states().next().expect("one file");
        assert_eq!(decode(&state), "Xalpha");
    }

    #[test]
    fn states_yield_paths_relative_to_the_root() {
        // A rooted path keys the guard by its full path but must stream under its relative
        // path — the key the frontend addresses the file by.
        let full =
            Path::<FilePath>::try_from(("proj".to_string(), "src/main.cad".to_string())).unwrap();
        let store = BlockingMemIo::new();
        store.write_file(&full, "p = 1").unwrap();
        let mut guard = MemFileGuard::new(&store);
        guard.load_all(&[full]);

        let streamer = MemFileStreamer::new(&guard);
        let (path, _) = streamer.states().next().expect("one file");

        assert_eq!(path, "src/main.cad");
    }

    #[test]
    fn empty_guard_streams_nothing() {
        let store = BlockingMemIo::new();
        let guard = MemFileGuard::new(&store);

        let streamer = MemFileStreamer::new(&guard);

        assert_eq!(streamer.count(), 0);
        assert!(streamer.is_empty());
        assert_eq!(streamer.states().count(), 0);
    }
}
