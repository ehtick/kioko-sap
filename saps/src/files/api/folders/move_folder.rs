use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::file::FileIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Moves a folder and its whole contents through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. After a successful
/// move the source folder no longer exists and its subtree lives at the destination.
///
/// # Arguments
/// * `handle`: The blocking IO backend to move through.
/// * `from`: The existing folder to move.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source is missing or the destination cannot
/// be written.
pub fn blocking<H: FolderIo>(
    handle: &H,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    handle.move_folder(from, to)
}

/// Moves a folder and its whole contents through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFolderIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to move through.
/// * `from`: The existing folder to move.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source is missing or the destination cannot
/// be written.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    handle.move_folder(from, to).await
}

/// Moves a folder through a blocking mem-file guard, carrying its buffers' unflushed edits.
///
/// The source subtree's buffers are dropped first, which flushes their edits to the store so
/// they travel with the move, then any stale buffers under the destination are evicted, then
/// the store move runs. `S: FileIo + FolderIo` so the one store reached through the guard covers
/// both the buffer eviction and the folder move.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `from`: The existing folder to move.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the store move fails.
pub fn memfile_blocking<S: FileIo + FolderIo>(
    guard: &mut MemFileGuard<'_, S>,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_dir(&from);
    guard.drop_dir(&to);
    store.move_folder(from, to)
}

/// Moves a folder through an async mem-file guard, carrying its buffers' unflushed edits.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo + AsyncFolderIo`, so
/// it serves the browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `from`: The existing folder to move.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the store move fails.
pub async fn memfile_asynchronous<S: AsyncFileIo + AsyncFolderIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_dir(&from).await;
    guard.drop_dir(&to).await;
    store.move_folder(from, to).await
}

#[cfg(test)]
mod tests {
    // See `child_files.rs` for why only the blocking path is covered here.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;
    use crate::files::io::file::FileIo;
    use crate::files::paths::FilePath;

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    #[test]
    fn blocking_moves_folder_subtree() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from/a.cad"), "a").unwrap();
        handle.write_file(&file("from/nested/b.cad"), "b").unwrap();

        blocking(&handle, folder("from"), folder("to")).unwrap();
        assert_eq!(handle.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(handle.read_file(&file("to/nested/b.cad")).unwrap(), "b");
        assert!(handle.read_file(&file("from/a.cad")).is_err());
    }

    #[test]
    fn blocking_missing_source_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, folder("missing"), folder("to")).is_err());
    }

    #[test]
    fn memfile_move_carries_unflushed_edits_and_clears_old_subtree() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from/a.cad"), "a").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        // A single char edit stays batched in a source-subtree buffer, not yet in the store.
        guard
            .get_file(&file("from/a.cad"))
            .unwrap()
            .insert_char(crate::kernel::transaction::CursorIndex { line: 0, col: 0 }, 'Z')
            .unwrap();

        memfile_blocking(&mut guard, folder("from"), folder("to")).unwrap();

        // The moved subtree carries the flushed edit and the old subtree is gone.
        assert_eq!(handle.read_file(&file("to/a.cad")).unwrap(), "Za");
        assert!(handle.read_file(&file("from/a.cad")).is_err());
        assert!(guard.file_map.is_empty());
    }
}
