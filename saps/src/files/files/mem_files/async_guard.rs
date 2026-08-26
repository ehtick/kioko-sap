//! The async counterpart to [`MemFileGuard`](super::guard::MemFileGuard): one live
//! [`MemFile`] buffer per open path, flushed through an async store.
//!
//! This mirrors the blocking guard exactly — same lazy loading, same one-buffer-per-path
//! sharing — but every operation that touches the store is awaited, because the backend is an
//! [`AsyncFileIo`] (browser IndexedDB, an async Postgres pool, and so on). Being generic over
//! that trait is the point: the same guard, and the same API endpoints built on it, work
//! against any async store without a backend-specific path.
//!
//! One behaviour differs from the blocking guard by necessity. The blocking buffers flush on
//! `Drop` as a safety net; an async buffer cannot, because `Drop` cannot await. So the async
//! guard has no drop-time flush: [`drop_file`](AsyncMemFileGuard::drop_file),
//! [`drop_dir`](AsyncMemFileGuard::drop_dir), and [`snapshot`](AsyncMemFileGuard::snapshot)
//! each await an explicit save, and a guard that is simply dropped persists nothing. Callers
//! that need durability before teardown must `snapshot` first.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::errors::file::FileError;
use crate::kernel::transaction::{
    CursorIndex, DeleteSlice, InsertSlice, Operation, SwapSlice, Transaction,
};

use crate::files::files::mem_files::full_mem_file::{Async, MemFile};
use crate::files::files::mem_files::guard::map_key;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::paths::{FilePath, FolderPath, Path};

/// Owns every file an async editing session has open in memory at once.
///
/// The async twin of [`MemFileGuard`](super::guard::MemFileGuard): generic over the async
/// store `S` it flushes through, borrowing it for the lifetime `'a` so a buffer never outlives
/// the store behind it. See the module docs for the one difference — there is no flush on drop.
pub struct AsyncMemFileGuard<'a, S: AsyncFileIo> {
    /// The live buffers, keyed by the full path each one reads from and writes back to.
    pub file_map: HashMap<PathBuf, MemFile<Async<'a, S>>>,
    /// The async store every buffer loads from and flushes to, borrowed for the guard's
    /// lifetime.
    store: &'a S,
}

impl<'a, S: AsyncFileIo> AsyncMemFileGuard<'a, S> {
    /// Builds an empty guard over `store`.
    ///
    /// # Arguments
    /// - `store`: The async store every buffer loads from and flushes to.
    ///
    /// # Returns
    /// An empty guard holding no buffers.
    pub fn new(store: &'a S) -> Self {
        Self { file_map: HashMap::new(), store }
    }

    /// Hands back the store every buffer flushes through, carrying the guard's `'a` lifetime so
    /// it can be held alongside a later `&mut` guard call (see the blocking guard's `store`).
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
    /// path. The eager counterpart to [`get_file`](Self::get_file).
    ///
    /// # Arguments
    /// - `path`: The file to load and store under its own full path as the map key.
    ///
    /// # Returns
    /// `Ok(())` once loaded and inserted, or the store's `FileError` if it cannot be read.
    pub async fn add_file(&mut self, path: &Path<FilePath>) -> Result<(), FileError> {
        let key = map_key(path);
        let file = MemFile::<Async<'_, S>>::from_file(path.clone(), Async::new(self.store)).await?;
        self.file_map.insert(key, file);
        Ok(())
    }

    /// Hands back the live buffer for a path, loading it from the store on first use.
    ///
    /// # Arguments
    /// - `path`: The file to fetch. Loaded from the store if not already open.
    ///
    /// # Returns
    /// A mutable reference to the buffer for `path`, or the store's `FileError` if a first time
    /// load fails.
    pub async fn get_file(
        &mut self,
        path: &Path<FilePath>,
    ) -> Result<&mut MemFile<Async<'a, S>>, FileError> {
        let key = map_key(path);
        if !self.file_map.contains_key(&key) {
            self.add_file(path).await?;
        }
        Ok(self.file_map.get_mut(&key).expect("file checked to exist above"))
    }

    /// Eagerly loads every file in `paths` into the map, skipping (and logging) any that cannot
    /// be read as UTF-8 text — a binary asset — rather than failing the whole load.
    ///
    /// # Arguments
    /// - `paths`: The project's file paths to load.
    pub async fn load_all(&mut self, paths: &[Path<FilePath>]) {
        for path in paths {
            if let Err(error) = self.add_file(path).await {
                tracing::warn!(
                    "async mem file guard skipping non-text file {}: {}",
                    String::from(path),
                    error
                );
            }
        }
    }

    /// Replaces the buffer for `path` with a fresh one seeded from `contents`, keeping the file
    /// resident. The whole-file overwrite counterpart to
    /// [`apply_transaction`](Self::apply_transaction).
    ///
    /// This is synchronous: seeding a buffer touches no store. Because an async buffer does not
    /// flush on drop, the replaced buffer's edits are simply discarded — the caller has already
    /// decided `contents` is the new truth — and the new text is not persisted until an
    /// explicit save (the write endpoint does this).
    ///
    /// # Arguments
    /// - `path`: The file whose buffer to replace, used as the map key.
    /// - `contents`: The new whole-file text to seed the buffer with.
    pub fn reset_file(&mut self, path: &Path<FilePath>, contents: String) {
        let key = map_key(path);
        self.file_map
            .insert(key, MemFile::from_string(path.clone(), Async::new(self.store), contents));
    }

    /// Flushes the buffer for `path` to the store, then drops it from the map. A path that is
    /// not open is a no-op.
    ///
    /// Unlike the blocking guard — where `Drop` does the flush — the save is explicit here,
    /// because an async buffer cannot flush on drop. The flush is best effort: a save error is
    /// swallowed so the buffer is still evicted.
    ///
    /// # Arguments
    /// - `path`: The file to flush and drop from the map.
    pub async fn drop_file(&mut self, path: &Path<FilePath>) {
        if let Some(mut file) = self.file_map.remove(&map_key(path)) {
            let _ = file.save().await;
        }
    }

    /// Flushes and drops every open buffer at or under `dir`, so a later operation cannot
    /// resurrect a file a directory move or delete has relocated or removed.
    ///
    /// Matching is by path component (via [`Path::starts_with`](std::path::Path::starts_with)),
    /// so a buffer under `dir` is dropped while a sibling that merely shares a name prefix is
    /// kept. The saves are awaited (so cannot run inside `retain`), so the matching keys are
    /// collected first, then each is flushed and removed.
    ///
    /// # Arguments
    /// - `dir`: The directory whose open buffers should be flushed and dropped.
    pub async fn drop_dir(&mut self, dir: &Path<FolderPath>) {
        let dir_full: &PathBuf = dir.into();
        let keys: Vec<PathBuf> =
            self.file_map.keys().filter(|key| key.starts_with(dir_full)).cloned().collect();
        for key in keys {
            if let Some(mut file) = self.file_map.remove(&key) {
                let _ = file.save().await;
            }
        }
    }

    /// Applies one transaction to the file at `path`, loading it on first use.
    ///
    /// # Arguments
    /// - `path`: The file to apply the transaction to.
    /// - `transaction`: The ordered list of slice edits to apply in order.
    ///
    /// # Returns
    /// The `yrs` update this transaction produced (the CRDT diff a replica applies to
    /// converge), or a `FileError` if the file cannot be loaded, an edit fails, or the diff
    /// cannot be encoded.
    pub async fn apply_transaction(
        &mut self,
        path: &Path<FilePath>,
        transaction: &Transaction,
    ) -> Result<Vec<u8>, FileError> {
        let file = self.get_file(path).await?;
        let before = file.state_vector();
        apply_transaction_ops(file, transaction).await?;
        file.encode_diff(&before)
    }

    /// Applies a series of transactions to the file at `path`, in order. The first failure
    /// stops the series; transactions before it have already been applied.
    ///
    /// # Arguments
    /// - `path`: The file to apply the transactions to.
    /// - `transactions`: The transactions to apply, in order.
    ///
    /// # Returns
    /// `Ok(())` once every transaction has been applied, or the first error as a `FileError`.
    pub async fn apply_transactions(
        &mut self,
        path: &Path<FilePath>,
        transactions: &[Transaction],
    ) -> Result<(), FileError> {
        for transaction in transactions {
            self.apply_transaction(path, transaction).await?;
        }
        Ok(())
    }

    /// Flushes every open buffer to the store in one pass, giving a single point at which the
    /// whole session is made durable. The first buffer that fails to save stops the pass.
    ///
    /// # Returns
    /// `Ok(())` once every buffer has been saved, or the first save error as a `FileError`.
    pub async fn snapshot(&mut self) -> Result<(), FileError> {
        for file in self.file_map.values_mut() {
            file.save().await?;
        }
        Ok(())
    }
}

// MARK: - Transaction application

/// Applies every operation in `transaction` to `file` in list order, persisting through the
/// buffer's own batched async save path. The async twin of the blocking guard's helper.
async fn apply_transaction_ops<S: AsyncFileIo>(
    file: &mut MemFile<Async<'_, S>>,
    transaction: &Transaction,
) -> Result<(), FileError> {
    for operation in transaction.operations() {
        match operation {
            Operation::Insert(InsertSlice { position, content }) => {
                apply_insert(file, *position, content).await?
            },
            Operation::Delete(DeleteSlice { position, length }) => {
                apply_delete(file, *position, *length).await?
            },
            Operation::Swap(SwapSlice { position, length, content }) => {
                apply_delete(file, *position, *length).await?;
                apply_insert(file, *position, content).await?;
            },
        }
    }
    Ok(())
}

/// Inserts `content` at `position`, choosing the buffer path by run length: a single character
/// takes the buffered `insert_char` path, a longer run the immediate `insert_text` path, and an
/// empty run is nothing to do.
async fn apply_insert<S: AsyncFileIo>(
    file: &mut MemFile<Async<'_, S>>,
    position: CursorIndex,
    content: &str,
) -> Result<(), FileError> {
    let mut characters = content.chars();
    match (characters.next(), characters.next()) {
        (None, _) => Ok(()),
        (Some(character), None) => file.insert_char(position, character).await,
        (Some(_), Some(_)) => file.insert_text(position, content).await,
    }
}

/// Deletes `length` characters from `position`, choosing the buffer path by run length: one
/// character takes the buffered `delete_char` path, a longer run the immediate `delete_range`
/// path, and a zero length run is nothing to do.
async fn apply_delete<S: AsyncFileIo>(
    file: &mut MemFile<Async<'_, S>>,
    position: CursorIndex,
    length: usize,
) -> Result<(), FileError> {
    match length {
        0 => Ok(()),
        1 => file.delete_char(position).await,
        _ => file.delete_range(position, length).await,
    }
}

// MARK: - Tests

#[cfg(test)]
mod tests {
    // Mirror the blocking guard's tests over an `AsyncMemIo`, driven to completion with
    // `block_on`, so the same lazy-load, batching, and eviction behaviour is checked on the
    // awaited write path. Reading the store back is what proves what actually reached it.

    use super::*;
    use crate::files::engines::async_io::mem::AsyncMemIo;
    use futures::executor::block_on;

    const START: &str = "the cat sat on the mat";

    fn cursor(line: usize, col: usize) -> CursorIndex {
        CursorIndex { line, col }
    }

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    /// Builds a store seeded with the given `(name, contents)` files.
    async fn seeded_store(files: &[(&str, &str)]) -> AsyncMemIo {
        let store = AsyncMemIo::new();
        for (name, contents) in files {
            store.write_file(&file(name), *contents).await.unwrap();
        }
        store
    }

    /// Reads `name` straight out of the store and asserts its whole contents.
    async fn assert_stored(store: &AsyncMemIo, name: &str, expected: &str) {
        assert_eq!(store.read_file(&file(name)).await.unwrap(), expected);
    }

    #[test]
    fn get_file_lazily_loads_and_edits_flush() {
        block_on(async {
            let store = seeded_store(&[("one.txt", START)]).await;
            let mut guard = AsyncMemFileGuard::new(&store);
            assert!(guard.file_map.is_empty());

            // A bulk insert flushes to the store straight away.
            guard
                .get_file(&file("one.txt"))
                .await
                .unwrap()
                .insert_text(cursor(0, 0), "X")
                .await
                .unwrap();

            assert!(guard.file_map.contains_key(&map_key(&file("one.txt"))));
            assert_stored(&store, "one.txt", "Xthe cat sat on the mat").await;
        });
    }

    #[test]
    fn add_file_missing_path_errors() {
        block_on(async {
            let store = AsyncMemIo::new();
            let mut guard = AsyncMemFileGuard::new(&store);
            assert!(guard.add_file(&file("nope.txt")).await.is_err());
            assert!(guard.file_map.is_empty());
        });
    }

    #[test]
    fn drop_file_flushes_then_evicts() {
        block_on(async {
            let store = seeded_store(&[("one.txt", START)]).await;
            let mut guard = AsyncMemFileGuard::new(&store);

            // A single char edit stays batched below the save threshold, so the store is
            // untouched until the explicit flush on drop.
            guard
                .get_file(&file("one.txt"))
                .await
                .unwrap()
                .insert_char(cursor(0, 0), 'X')
                .await
                .unwrap();
            assert_stored(&store, "one.txt", START).await;

            guard.drop_file(&file("one.txt")).await;

            assert_stored(&store, "one.txt", "Xthe cat sat on the mat").await;
            assert!(!guard.file_map.contains_key(&map_key(&file("one.txt"))));
        });
    }

    #[test]
    fn drop_dir_flushes_and_evicts_subtree_only() {
        block_on(async {
            let store =
                seeded_store(&[("sub/a.txt", "a"), ("subway.txt", "s"), ("other.txt", "o")]).await;
            let mut guard = AsyncMemFileGuard::new(&store);
            for name in ["sub/a.txt", "subway.txt", "other.txt"] {
                guard.get_file(&file(name)).await.unwrap();
            }

            guard.drop_dir(&Path::<FolderPath>::new("sub").unwrap()).await;

            assert!(!guard.file_map.contains_key(&map_key(&file("sub/a.txt"))));
            assert!(guard.file_map.contains_key(&map_key(&file("subway.txt"))));
            assert!(guard.file_map.contains_key(&map_key(&file("other.txt"))));
        });
    }

    #[test]
    fn apply_transaction_edits_and_returns_diff() {
        block_on(async {
            let store = seeded_store(&[("one.txt", START)]).await;
            let mut guard = AsyncMemFileGuard::new(&store);

            let transaction = Transaction::new(vec![Operation::Insert(InsertSlice {
                position: cursor(0, 0),
                content: "hello ".to_string(),
            })]);
            let diff = guard.apply_transaction(&file("one.txt"), &transaction).await.unwrap();

            assert_stored(&store, "one.txt", "hello the cat sat on the mat").await;
            assert!(!diff.is_empty());
        });
    }

    #[test]
    fn reset_file_reseeds_and_snapshot_persists() {
        block_on(async {
            let store = seeded_store(&[("one.txt", "old")]).await;
            let mut guard = AsyncMemFileGuard::new(&store);

            guard.reset_file(&file("one.txt"), "brand new".into());
            // reset alone does not persist (async buffers do not flush on their own here).
            assert_stored(&store, "one.txt", "old").await;

            guard.snapshot().await.unwrap();
            assert_stored(&store, "one.txt", "brand new").await;
        });
    }

    #[test]
    fn snapshot_saves_all_buffers() {
        block_on(async {
            let store = seeded_store(&[("one.txt", "one"), ("two.txt", "two")]).await;
            let mut guard = AsyncMemFileGuard::new(&store);

            guard
                .get_file(&file("one.txt"))
                .await
                .unwrap()
                .insert_char(cursor(0, 0), 'A')
                .await
                .unwrap();
            guard
                .get_file(&file("two.txt"))
                .await
                .unwrap()
                .insert_char(cursor(0, 0), 'B')
                .await
                .unwrap();
            assert_stored(&store, "one.txt", "one").await;

            guard.snapshot().await.unwrap();

            assert_stored(&store, "one.txt", "Aone").await;
            assert_stored(&store, "two.txt", "Btwo").await;
        });
    }
}
