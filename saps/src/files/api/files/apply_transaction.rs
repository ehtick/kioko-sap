use crate::errors::file::FileError;
use crate::kernel::transaction::Transaction;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Applies a transaction to a file through a blocking mem-file guard.
///
/// This is the mem-file specific editing path — there is no store-level counterpart, because a
/// transaction is a batch of slice edits against the live buffer, not a whole-file write. Each
/// operation is resolved against the buffer as it stands when it runs, and the buffer flushes
/// to the store as edits cross its save threshold. The file is loaded on first use.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `path`: The file to apply the transaction to.
/// * `transaction`: The ordered slice edits to apply.
///
/// # Returns
/// The `yrs` update the transaction produced — the CRDT diff a replica applies to converge —
/// or a `FileError` if the file cannot be loaded, an edit fails, or the diff cannot be encoded.
pub fn memfile_blocking<S: FileIo>(
    guard: &mut MemFileGuard<'_, S>,
    path: &Path<FilePath>,
    transaction: &Transaction,
) -> Result<Vec<u8>, FileError> {
    guard.apply_transaction(path, transaction)
}

/// Applies a transaction to a file through an async mem-file guard.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `path`: The file to apply the transaction to.
/// * `transaction`: The ordered slice edits to apply.
///
/// # Returns
/// The `yrs` update the transaction produced, or a `FileError` if the file cannot be loaded, an
/// edit fails, or the diff cannot be encoded.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    path: &Path<FilePath>,
    transaction: &Transaction,
) -> Result<Vec<u8>, FileError> {
    guard.apply_transaction(path, transaction).await
}

#[cfg(test)]
mod tests {
    // The async path is generic over `AsyncFileIo`, whose only host-testable backend is the
    // async in-memory fake exercised in the async guard tests, so these host tests cover the
    // blocking path through the in-memory handle.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;
    use crate::files::io::file::FileIo;
    use crate::kernel::transaction::{CursorIndex, InsertSlice, Operation};

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn memfile_applies_transaction_and_returns_diff() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("main.cad"), "world").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        let transaction = Transaction::new(vec![Operation::Insert(InsertSlice {
            position: CursorIndex { line: 0, col: 0 },
            content: "hello ".to_string(),
        })]);
        let diff = memfile_blocking(&mut guard, &file("main.cad"), &transaction).unwrap();

        // A bulk insert flushes, so the store reflects the edit, and the diff is non-empty.
        assert_eq!(handle.read_file(&file("main.cad")).unwrap(), "hello world");
        assert!(!diff.is_empty());
    }

    #[test]
    fn memfile_missing_file_errors() {
        let handle = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&handle);
        let transaction = Transaction::new(vec![Operation::Insert(InsertSlice {
            position: CursorIndex { line: 0, col: 0 },
            content: "x".to_string(),
        })]);
        assert!(memfile_blocking(&mut guard, &file("missing.cad"), &transaction).is_err());
    }
}
