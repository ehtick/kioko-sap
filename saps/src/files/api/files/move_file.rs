use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Moves a file through a blocking IO handle.
///
/// The handle is any type implementing `FileIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. After a successful
/// move the source no longer exists and its contents live at the destination.
///
/// # Arguments
/// * `handle`: The blocking IO backend to move through.
/// * `from`: The existing file to move.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source is missing or the destination cannot
/// be written.
pub fn blocking<H: FileIo>(
    handle: &H,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    handle.move_file(from, to)
}

/// Moves a file through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFileIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to move through.
/// * `from`: The existing file to move.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source is missing or the destination cannot
/// be written.
pub async fn asynchronous<H: AsyncFileIo>(
    handle: &H,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    handle.move_file(from, to).await
}

/// Moves a file through a blocking mem-file guard, carrying its unflushed edits with it.
///
/// The source buffer is dropped first, which flushes its edits to the store so they travel
/// with the file, then any stale buffer at the destination is evicted, then the store move
/// runs. Dropping the source before the move (and the destination too) is what stops a later
/// flush resurrecting either path.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `from`: The existing file to move.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the store move fails.
pub fn memfile_blocking<S: FileIo>(
    guard: &mut MemFileGuard<'_, S>,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_file(from);
    guard.drop_file(to);
    store.move_file(from, to)
}

/// Moves a file through an async mem-file guard, carrying its unflushed edits with it.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `from`: The existing file to move.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the store move fails.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_file(from).await;
    guard.drop_file(to).await;
    store.move_file(from, to).await
}

#[cfg(test)]
mod tests {
    // See `read.rs` for why only the blocking path is covered here.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn blocking_moves_through_handle() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from.cad"), "payload").unwrap();
        blocking(&handle, &file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(handle.read_file(&file("to.cad")).unwrap(), "payload");
        assert!(handle.read_file(&file("from.cad")).is_err());
    }

    #[test]
    fn blocking_missing_source_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, &file("missing.cad"), &file("to.cad")).is_err());
    }

    #[test]
    fn memfile_move_carries_unflushed_edits_and_clears_old_path() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from.cad"), "payload").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        // A single char edit stays batched in the buffer, not yet in the store.
        guard
            .get_file(&file("from.cad"))
            .unwrap()
            .insert_char(crate::kernel::transaction::CursorIndex { line: 0, col: 0 }, 'Z')
            .unwrap();

        memfile_blocking(&mut guard, &file("from.cad"), &file("to.cad")).unwrap();

        // The moved file carries the flushed edit, and the old path is gone.
        assert_eq!(handle.read_file(&file("to.cad")).unwrap(), "Zpayload");
        assert!(handle.read_file(&file("from.cad")).is_err());
        assert!(!guard.file_map.contains_key(std::path::Path::new("from.cad")));
    }
}
