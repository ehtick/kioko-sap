//! The in-memory file store for an editing session: one live [`MemFile`] buffer per open path.
//!
//! A [`MemFile`] is the in-memory buffer for a single file (see the `full_mem_file` module).
//! The guard is the layer above it: it holds a buffer per path so the rest of the system asks
//! for a file by its path and gets back the one live buffer for it, rather than each caller
//! loading its own copy and the copies drifting apart.
//!
//! Every buffer flushes through the same blocking [`FileIo`] store, borrowed for the guard's
//! lifetime, so a save and the flush-on-drop write back to that store. Files load lazily: a
//! path is only read the first time it is asked for, and from then on the same buffer is
//! reused — carrying any unsaved edits — until the guard drops it.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::errors::file::FileError;
use crate::kernel::transaction::{
    CursorIndex, DeleteSlice, InsertSlice, Operation, SwapSlice, Transaction,
};

use crate::files::files::mem_files::full_mem_file::{Blocking, MemFile};
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, FolderPath, Path};

/// The map key for a buffer: its full path (root joined with the relative path), which is
/// what the store reads and writes. Deriving it through the public borrow keeps the guard off
/// the path's private fields.
pub(crate) fn map_key(path: &Path<FilePath>) -> PathBuf {
    let full: &PathBuf = path.into();
    full.clone()
}

/// Owns every file an editing session has open in memory at once.
///
/// The guard is generic over the blocking store `S` it flushes through, and borrows that store
/// for the lifetime `'a` so every buffer it builds writes back to the same place. Binary
/// assets that cannot be held as UTF-8 text are not the guard's concern — [`load_all`] skips
/// them and callers serve them straight from the store on demand instead.
///
/// [`load_all`]: MemFileGuard::load_all
pub struct MemFileGuard<'a, S: FileIo> {
    /// The live buffers, keyed by the full path each one reads from and writes back to. One
    /// entry per file currently open in this session.
    pub file_map: HashMap<PathBuf, MemFile<Blocking<'a, S>>>,
    /// The blocking store every buffer loads from and flushes to, borrowed for the guard's
    /// lifetime so a buffer never outlives the store behind it.
    store: &'a S,
}

impl<'a, S: FileIo> MemFileGuard<'a, S> {
    /// Builds an empty guard over `store`.
    ///
    /// No file is loaded up front; each is read from `store` on first use (see
    /// [`get_file`](Self::get_file)) unless [`load_all`](Self::load_all) is called to make the
    /// whole project resident at once.
    ///
    /// # Arguments
    /// - `store`: The blocking store every buffer loads from and flushes to.
    ///
    /// # Returns
    /// An empty guard holding no buffers.
    pub fn new(store: &'a S) -> Self {
        Self { file_map: HashMap::new(), store }
    }

    /// Hands back the store every buffer flushes through.
    ///
    /// The returned reference carries the guard's own `'a` lifetime, not the borrow of `&self`,
    /// so a caller can hold it alongside a later `&mut` guard call. This lets the higher level
    /// API endpoints reach the store for the parts of an operation the buffer layer does not
    /// cover — deleting, moving, or copying the durable file — while the guard handles the
    /// buffer coordination around it.
    ///
    /// # Returns
    /// The store the guard was built over.
    pub fn store(&self) -> &'a S {
        self.store
    }

    /// Reports whether a live buffer is held for a path.
    ///
    /// A buffer can exist for a file the store does not hold yet - a `reset_file` that has
    /// not flushed - so residency is its own existence signal alongside the store's.
    ///
    /// # Arguments
    /// - `path`: The file to check for.
    ///
    /// # Returns
    /// `true` if a buffer is held for `path`, otherwise `false`.
    pub fn is_resident(&self, path: &Path<FilePath>) -> bool {
        self.file_map.contains_key(&map_key(path))
    }

    /// Loads a file from the store into the map, replacing any buffer already held for that
    /// path.
    ///
    /// The path is read straight away so the buffer is populated on return. This is the eager
    /// path; most callers should go through [`get_file`](Self::get_file), which only loads a
    /// path the first time it is needed.
    ///
    /// # Arguments
    /// - `path`: The file to load and store under its own full path as the map key.
    ///
    /// # Returns
    /// `Ok(())` once the file is loaded and inserted, or the store's `FileError` if the file
    /// cannot be read (for example a binary asset that is not valid UTF-8).
    pub fn add_file(&mut self, path: &Path<FilePath>) -> Result<(), FileError> {
        let key = map_key(path);
        let file = MemFile::<Blocking<'_, S>>::from_file(path.clone(), Blocking::new(self.store))?;
        self.file_map.insert(key, file);
        Ok(())
    }

    /// Hands back the live buffer for a path, loading it on first use.
    ///
    /// This is the main way callers reach a file. If the path has not been opened yet it is
    /// loaded from the store and kept; if it has, the existing buffer is returned so earlier
    /// in-memory edits are still there. Either way the caller gets a mutable handle to the one
    /// buffer the session shares for that path.
    ///
    /// # Arguments
    /// - `path`: The file to fetch. Loaded from the store if not already open.
    ///
    /// # Returns
    /// A mutable reference to the buffer for `path`, or the store's `FileError` if a first
    /// time load fails.
    pub fn get_file(
        &mut self,
        path: &Path<FilePath>,
    ) -> Result<&mut MemFile<Blocking<'a, S>>, FileError> {
        let key = map_key(path);
        if !self.file_map.contains_key(&key) {
            self.add_file(path)?;
        }
        Ok(self.file_map.get_mut(&key).expect("file checked to exist above"))
    }

    /// Eagerly loads every file in `paths` into the map, so the whole project is resident in
    /// memory for the session rather than loaded lazily on first touch.
    ///
    /// Best effort: a path that cannot be read as UTF-8 text — a binary asset — is logged and
    /// skipped rather than failing the whole load. Skipped files are served straight from the
    /// store on demand instead. Only files belong in the map, so the caller filters directory
    /// entries out before passing them here.
    ///
    /// # Arguments
    /// - `paths`: The project's file paths to load.
    pub fn load_all(&mut self, paths: &[Path<FilePath>]) {
        for path in paths {
            if let Err(error) = self.add_file(path) {
                tracing::warn!(
                    "mem file guard skipping non-text file {}: {}",
                    String::from(path),
                    error
                );
            }
        }
    }

    /// Replaces the buffer for `path` with a fresh one seeded from `contents`, keeping the
    /// file resident in memory.
    ///
    /// This is the full-overwrite counterpart to [`apply_transaction`](Self::apply_transaction):
    /// a whole-file write replaces the text outright, so the buffer is re-seeded rather than
    /// edited in place. Unlike [`drop_file`](Self::drop_file) it does **not** evict — the file
    /// stays in memory as the session's source of truth, so the next read serves the new
    /// content from memory without a store round trip.
    ///
    /// # Arguments
    /// - `path`: The file whose buffer to replace, used as the map key.
    /// - `contents`: The new whole-file text to seed the buffer with.
    pub fn reset_file(&mut self, path: &Path<FilePath>, contents: String) {
        let key = map_key(path);
        // Drop the old buffer with its flush-on-drop DISABLED first: the caller has already
        // written `contents` to the store, so letting the old buffer flush on drop would write
        // its stale previous text straight back over that write.
        if let Some(mut old) = self.file_map.remove(&key) {
            old.flush_on_drop = false;
        }
        self.file_map
            .insert(key, MemFile::from_string(path.clone(), Blocking::new(self.store), contents));
    }

    /// Drops the buffer for `path` from the map, flushing it to the store on the way out.
    ///
    /// Use this to close a file out of the session. There is no separate save step: the
    /// buffer flushes itself in its `Drop` impl, so removing it from the map both releases the
    /// memory and persists any unsaved edits. The trade off is that a failure on that final
    /// flush is swallowed rather than returned, so this is a best effort save. A path that is
    /// not open is a no-op.
    ///
    /// # Arguments
    /// - `path`: The file to flush and drop from the map.
    pub fn drop_file(&mut self, path: &Path<FilePath>) {
        self.file_map.remove(&map_key(path));
    }

    /// Drops every open buffer at or under `dir`, so a later flush cannot resurrect a file
    /// that a directory move or delete has just relocated or removed.
    ///
    /// Matching is by path component (via [`Path::starts_with`]), so a buffer under `dir` is
    /// dropped while a sibling that merely shares a name prefix is kept. A `dir` with no open
    /// buffers under it is a no-op. This is the directory counterpart to
    /// [`drop_file`](Self::drop_file): a move or delete of a whole directory must drop every
    /// buffer inside it, not just one path.
    ///
    /// # Arguments
    /// - `dir`: The directory whose open buffers should be dropped.
    pub fn drop_dir(&mut self, dir: &Path<FolderPath>) {
        let dir_full: &PathBuf = dir.into();
        self.file_map.retain(|key, _| !key.starts_with(dir_full));
    }

    /// Applies one transaction to the file at `path`.
    ///
    /// The file is loaded on first use through [`get_file`](Self::get_file), so a transaction
    /// can target a path that has not been opened yet. A single character edit batches in the
    /// buffer towards the save threshold; a longer slice flushes to the store as it runs (see
    /// the blocking write path).
    ///
    /// # Arguments
    /// - `path`: The file to apply the transaction to.
    /// - `transaction`: The ordered list of slice edits to apply in order.
    ///
    /// # Returns
    /// The `yrs` update this transaction produced — the minimal CRDT diff from the buffer's
    /// pre-apply state to now, encoded with [`MemFile::encode_diff`]. A replica applies it
    /// with `apply_update` to reach the identical state, which is what an endpoint fans out to
    /// the other users. A failure to load the file, apply an edit, or encode the diff is
    /// returned as a `FileError`.
    pub fn apply_transaction(
        &mut self,
        path: &Path<FilePath>,
        transaction: &Transaction,
    ) -> Result<Vec<u8>, FileError> {
        let file = self.get_file(path)?;
        // Snapshot the buffer's state vector BEFORE applying so we can encode the exact update
        // this transaction adds (the diff from here to post-apply), rather than re-sending the
        // whole document.
        let before = file.state_vector();
        apply_transaction_ops(file, transaction)?;
        file.encode_diff(&before)
    }

    /// Applies a series of transactions to the file at `path`, in order.
    ///
    /// This is the batched editing path: each transaction is an ordered list of slice edits,
    /// and the buffer flushes itself to the store as single character edits cross its save
    /// threshold and as bulk slices run, so a long series of keystroke transactions turns into
    /// a small number of writes rather than one per edit. The first transaction that fails
    /// stops the series; transactions before it have already been applied.
    ///
    /// # Arguments
    /// - `path`: The file to apply the transactions to.
    /// - `transactions`: The transactions to apply, in order.
    ///
    /// # Returns
    /// `Ok(())` once every transaction has been applied, or the first error as a `FileError`.
    pub fn apply_transactions(
        &mut self,
        path: &Path<FilePath>,
        transactions: &[Transaction],
    ) -> Result<(), FileError> {
        for transaction in transactions {
            self.apply_transaction(path, transaction)?;
        }
        Ok(())
    }

    /// Flushes every open buffer to the store in one pass.
    ///
    /// Single character edits are held in memory until enough build up (see the blocking write
    /// path), so at any moment some buffers can hold edits the store has not seen. This walks
    /// all of them and forces a save, giving a single point at which the whole session is made
    /// durable. The first buffer that fails to save stops the pass and its error is returned.
    ///
    /// # Returns
    /// `Ok(())` once every buffer has been saved, or the first save error as a `FileError`.
    pub fn snapshot(&mut self) -> Result<(), FileError> {
        for file in self.file_map.values_mut() {
            file.save()?;
        }
        Ok(())
    }
}

// MARK: - Transaction application

/// Applies every operation in `transaction` to `file` in list order.
///
/// Each operation is resolved against the buffer as it stands at the moment it runs, not
/// against the buffer as it was when the transaction was built. So an operation's position
/// must account for the inserts, deletes, and swaps of the operations before it in the same
/// transaction. A swap is applied as a delete of its run followed by an insert of the
/// replacement at the same position, so the replacement may be a different length from the run
/// it replaces.
///
/// # Arguments
/// - `file`: The buffer to apply the operations to.
/// - `transaction`: The ordered list of slice edits to apply.
///
/// # Returns
/// `Ok(())` once every operation has been applied. The first edit that fails stops the
/// transaction and returns its `FileError`; any earlier edits have already been applied.
fn apply_transaction_ops<S: FileIo>(
    file: &mut MemFile<Blocking<'_, S>>,
    transaction: &Transaction,
) -> Result<(), FileError> {
    for operation in transaction.operations() {
        match operation {
            Operation::Insert(InsertSlice { position, content }) => {
                apply_insert(file, *position, content)?
            },
            Operation::Delete(DeleteSlice { position, length }) => {
                apply_delete(file, *position, *length)?
            },
            Operation::Swap(SwapSlice { position, length, content }) => {
                apply_delete(file, *position, *length)?;
                apply_insert(file, *position, content)?;
            },
        }
    }
    Ok(())
}

/// Inserts `content` at `position`, choosing the buffer path by run length: a single character
/// takes the buffered `insert_char` path, a longer run the immediate `insert_text` path, and
/// an empty run is nothing to do.
fn apply_insert<S: FileIo>(
    file: &mut MemFile<Blocking<'_, S>>,
    position: CursorIndex,
    content: &str,
) -> Result<(), FileError> {
    let mut characters = content.chars();
    match (characters.next(), characters.next()) {
        (None, _) => Ok(()),
        (Some(character), None) => file.insert_char(position, character),
        (Some(_), Some(_)) => file.insert_text(position, content),
    }
}

/// Deletes `length` characters from `position`, choosing the buffer path by run length: one
/// character takes the buffered `delete_char` path, a longer run the immediate `delete_range`
/// path, and a zero length run is nothing to do.
fn apply_delete<S: FileIo>(
    file: &mut MemFile<Blocking<'_, S>>,
    position: CursorIndex,
    length: usize,
) -> Result<(), FileError> {
    match length {
        0 => Ok(()),
        1 => file.delete_char(position),
        _ => file.delete_range(position, length),
    }
}

// MARK: - Tests

#[cfg(test)]
mod tests {
    // Each test builds a guard over a `BlockingMemIo` seeded with known files, drives one guard
    // method, and reads the store back to assert what actually persisted — so the batching and
    // flush-on-drop policy is checked, not just the in-memory text.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;

    const START: &str = "the cat sat on the mat";

    fn cursor(line: usize, col: usize) -> CursorIndex {
        CursorIndex { line, col }
    }

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    /// Builds a store seeded with the given `(name, contents)` files.
    fn seeded_store(files: &[(&str, &str)]) -> BlockingMemIo {
        let store = BlockingMemIo::new();
        for (name, contents) in files {
            store.write_file(&file(name), *contents).unwrap();
        }
        store
    }

    /// Reads `name` straight out of the store and asserts its whole contents.
    fn assert_stored(store: &BlockingMemIo, name: &str, expected: &str) {
        assert_eq!(store.read_file(&file(name)).unwrap(), expected);
    }

    /// Adding a file loads it into the map keyed by its full path.
    #[test]
    fn add_file_inserts_into_map() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        guard.add_file(&file("one.txt")).expect("add file");

        assert!(guard.file_map.contains_key(&map_key(&file("one.txt"))));
        assert_eq!(1, guard.file_map.len());
    }

    /// Adding a path the store does not hold surfaces an error and leaves the map untouched.
    #[test]
    fn add_file_missing_path_errors() {
        let store = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&store);

        assert!(guard.add_file(&file("nope.txt")).is_err());
        assert!(guard.file_map.is_empty());
    }

    /// Getting a file that has not been loaded yet lazily adds it and hands back a mutable
    /// handle to the freshly loaded buffer.
    #[test]
    fn get_file_lazily_loads_unknown_path() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);
        assert!(guard.file_map.is_empty());

        // Mutate through the handle; a bulk insert flushes to the store straight away.
        guard.get_file(&file("one.txt")).expect("get file").insert_text(cursor(0, 0), "X").unwrap();

        assert!(guard.file_map.contains_key(&map_key(&file("one.txt"))));
        assert_stored(&store, "one.txt", "Xthe cat sat on the mat");
    }

    /// A second `get_file` for the same path returns the already loaded buffer rather than
    /// reloading from the store, so unsaved in-memory edits are kept.
    #[test]
    fn get_file_returns_existing_buffer() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        // First access edits the buffer; the single char edit stays in memory (below the save
        // threshold).
        guard.get_file(&file("one.txt")).expect("get file").insert_char(cursor(0, 0), 'X').unwrap();

        // Second access must hand back the same buffer holding that edit, not a fresh reload.
        guard.get_file(&file("one.txt")).expect("get file").insert_text(cursor(0, 0), "Y").unwrap();

        // insert_text flushes, so both edits land in the store together.
        assert_eq!(1, guard.file_map.len());
        assert_stored(&store, "one.txt", "YXthe cat sat on the mat");
    }

    /// Getting a path the store does not hold surfaces an error.
    #[test]
    fn get_file_missing_path_errors() {
        let store = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&store);

        assert!(guard.get_file(&file("nope.txt")).is_err());
        assert!(guard.file_map.is_empty());
    }

    /// `load_all` makes the listed files resident and skips a path it cannot read rather than
    /// failing the whole load.
    #[test]
    fn load_all_loads_present_files_and_skips_missing() {
        let store = seeded_store(&[("one.txt", "a"), ("two.txt", "b")]);
        let mut guard = MemFileGuard::new(&store);

        // "missing.txt" is not in the store; it is skipped, the other two still load.
        guard.load_all(&[file("one.txt"), file("missing.txt"), file("two.txt")]);

        assert_eq!(2, guard.file_map.len());
        assert!(guard.file_map.contains_key(&map_key(&file("one.txt"))));
        assert!(guard.file_map.contains_key(&map_key(&file("two.txt"))));
        assert!(!guard.file_map.contains_key(&map_key(&file("missing.txt"))));
    }

    /// Resetting a file replaces the buffer with the new contents and, crucially, does not let
    /// the stale old buffer flush back over the caller's whole-file write.
    #[test]
    fn reset_file_reseeds_without_clobbering_the_store() {
        let store = seeded_store(&[("one.txt", "old")]);
        let mut guard = MemFileGuard::new(&store);

        // Buffer a single char edit so the resident buffer ("Xold") differs from the store.
        guard.get_file(&file("one.txt")).expect("get file").insert_char(cursor(0, 0), 'X').unwrap();
        // The caller writes the whole file to the store, then resets the guard to match.
        store.write_file(&file("one.txt"), "brand new").unwrap();

        guard.reset_file(&file("one.txt"), "brand new".into());

        // The dropped stale buffer did NOT flush "Xold" back over the caller's write.
        assert_stored(&store, "one.txt", "brand new");
        // The buffer is still resident and serves the new content from memory.
        assert_eq!(guard.get_file(&file("one.txt")).expect("get file").contents(), "brand new");
    }

    /// Dropping a file flushes its in-memory edits to the store (via the buffer's `Drop`) and
    /// removes it from the map.
    #[test]
    fn drop_file_flushes_and_drops() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        // A single char edit stays in memory (below the save threshold), so the store is
        // untouched until the buffer is dropped.
        guard.get_file(&file("one.txt")).expect("get file").insert_char(cursor(0, 0), 'X').unwrap();
        assert_stored(&store, "one.txt", START);

        guard.drop_file(&file("one.txt"));

        // Dropping the buffer flushed the edit and released it from the map.
        assert_stored(&store, "one.txt", "Xthe cat sat on the mat");
        assert!(!guard.file_map.contains_key(&map_key(&file("one.txt"))));
    }

    /// Dropping a path that is not open is a no-op.
    #[test]
    fn drop_file_unknown_path_is_noop() {
        let store = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&store);
        guard.drop_file(&file("not_open.txt"));
        assert!(guard.file_map.is_empty());
    }

    /// Dropping a directory removes every buffer at or under it, keeping a sibling that only
    /// shares a name prefix.
    #[test]
    fn drop_dir_drops_buffers_under_it_only() {
        let store = seeded_store(&[
            ("sub/a.txt", "a"),
            ("sub/b.txt", "b"),
            ("subway.txt", "s"),
            ("other.txt", "o"),
        ]);
        let mut guard = MemFileGuard::new(&store);
        for name in ["sub/a.txt", "sub/b.txt", "subway.txt", "other.txt"] {
            guard.get_file(&file(name)).expect("get file");
        }

        guard.drop_dir(&folder("sub"));

        // Both buffers under "sub" are gone; the prefix-sharing sibling and the unrelated file
        // are kept (the match is on path components, not a string prefix).
        assert!(!guard.file_map.contains_key(&map_key(&file("sub/a.txt"))));
        assert!(!guard.file_map.contains_key(&map_key(&file("sub/b.txt"))));
        assert!(guard.file_map.contains_key(&map_key(&file("subway.txt"))));
        assert!(guard.file_map.contains_key(&map_key(&file("other.txt"))));
    }

    /// A single transaction applies to a file and returns a non-empty CRDT diff describing the
    /// change.
    #[test]
    fn apply_transaction_edits_file_and_returns_diff() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        let transaction = Transaction::new(vec![Operation::Insert(InsertSlice {
            position: cursor(0, 0),
            content: "hello ".to_string(),
        })]);
        let diff = guard.apply_transaction(&file("one.txt"), &transaction).expect("apply");

        // A bulk insert flushes, so the store already reflects the edit.
        assert_stored(&store, "one.txt", "hello the cat sat on the mat");
        // The returned diff is the CRDT update a replica would apply to converge.
        assert!(!diff.is_empty());
    }

    /// A series of transactions applies to one file in order, loading it on the first
    /// transaction and carrying the running edits through the rest.
    #[test]
    fn apply_transactions_runs_series_in_order() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        // First inserts "hi " at the front, second deletes the leading "the " that has now
        // shifted to column three.
        let first = Transaction::new(vec![Operation::Insert(InsertSlice {
            position: cursor(0, 0),
            content: "hi ".to_string(),
        })]);
        let second = Transaction::new(vec![Operation::Delete(DeleteSlice {
            position: cursor(0, 3),
            length: 4,
        })]);

        guard.apply_transactions(&file("one.txt"), &[first, second]).expect("apply transactions");
        guard.snapshot().expect("snapshot");

        assert_stored(&store, "one.txt", "hi cat sat on the mat");
    }

    /// A swap operation replaces its run with a different length run.
    #[test]
    fn apply_transaction_swap_replaces_run() {
        let store = seeded_store(&[("one.txt", START)]);
        let mut guard = MemFileGuard::new(&store);

        // Replace the three char "the" with the four char "wolf", so the file grows by one.
        let transaction = Transaction::new(vec![Operation::Swap(SwapSlice {
            position: cursor(0, 0),
            length: 3,
            content: "wolf".to_string(),
        })]);
        guard.apply_transaction(&file("one.txt"), &transaction).expect("apply");

        assert_stored(&store, "one.txt", "wolf cat sat on the mat");
    }

    /// `snapshot` flushes every buffer's in-memory edits to the store in one pass.
    #[test]
    fn snapshot_saves_all_buffers() {
        let store = seeded_store(&[("one.txt", "one"), ("two.txt", "two")]);
        let mut guard = MemFileGuard::new(&store);

        // Single char edits stay in memory (below the save threshold), so the store is
        // untouched until the snapshot.
        guard.get_file(&file("one.txt")).expect("get file").insert_char(cursor(0, 0), 'A').unwrap();
        guard.get_file(&file("two.txt")).expect("get file").insert_char(cursor(0, 0), 'B').unwrap();
        assert_stored(&store, "one.txt", "one");
        assert_stored(&store, "two.txt", "two");

        guard.snapshot().expect("snapshot");

        assert_stored(&store, "one.txt", "Aone");
        assert_stored(&store, "two.txt", "Btwo");
    }

    /// Dropping the whole guard flushes every resident buffer's buffered edits to the store —
    /// the session teardown safety net.
    #[test]
    fn dropping_the_guard_flushes_every_buffer() {
        let store = seeded_store(&[("one.txt", "one"), ("two.txt", "two")]);
        {
            let mut guard = MemFileGuard::new(&store);
            // Single char edits, all below the save threshold, so nothing has reached the store.
            guard
                .get_file(&file("one.txt"))
                .expect("get file")
                .insert_char(cursor(0, 0), 'A')
                .unwrap();
            guard
                .get_file(&file("two.txt"))
                .expect("get file")
                .insert_char(cursor(0, 0), 'B')
                .unwrap();
            assert_stored(&store, "one.txt", "one");
        } // The guard drops here, dropping each buffer, whose flush writes the edits through.

        assert_stored(&store, "one.txt", "Aone");
        assert_stored(&store, "two.txt", "Btwo");
    }
}
