use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::file::FileIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Copies a folder and its whole contents through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. After a successful
/// copy both folders exist and hold the same subtree.
///
/// # Arguments
/// * `handle`: The blocking IO backend to copy through.
/// * `from`: The existing folder to copy.
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
    handle.copy_folder(from, to)
}

/// Copies a folder and its whole contents through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFolderIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to copy through.
/// * `from`: The existing folder to copy.
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
    handle.copy_folder(from, to).await
}

/// Copies a folder through a blocking mem-file guard, copying the current text of every file
/// including unflushed edits.
///
/// Every buffer is flushed first via [`snapshot`](MemFileGuard::snapshot) so the source subtree
/// in the store reflects the latest edits — a copy must not miss text still sitting in a
/// buffer. Then any stale buffers under the destination are evicted so they cannot serve the
/// pre-copy content, and the store copy runs. `S: FileIo + FolderIo` so the one store reached
/// through the guard covers both.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `from`: The existing folder to copy.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if flushing the buffers or the store copy fails.
pub fn memfile_blocking<S: FileIo + FolderIo>(
    guard: &mut MemFileGuard<'_, S>,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.snapshot()?;
    guard.drop_dir(&to);
    store.copy_folder(from, to)
}

/// Copies a folder through an async mem-file guard, copying the current text of every file
/// including unflushed edits.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo + AsyncFolderIo`, so
/// it serves the browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `from`: The existing folder to copy.
/// * `to`: The destination folder path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if flushing the buffers or the store copy fails.
pub async fn memfile_asynchronous<S: AsyncFileIo + AsyncFolderIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    from: Path<FolderPath>,
    to: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.snapshot().await?;
    guard.drop_dir(&to).await;
    store.copy_folder(from, to).await
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
    fn blocking_copies_folder_subtree() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from/a.cad"), "a").unwrap();
        handle.write_file(&file("from/nested/b.cad"), "b").unwrap();

        blocking(&handle, folder("from"), folder("to")).unwrap();
        // Source is untouched.
        assert_eq!(handle.read_file(&file("from/a.cad")).unwrap(), "a");
        // Destination mirrors the whole subtree.
        assert_eq!(handle.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(handle.read_file(&file("to/nested/b.cad")).unwrap(), "b");
    }

    #[test]
    fn blocking_missing_source_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, folder("missing"), folder("to")).is_err());
    }

    #[test]
    fn memfile_copy_includes_unflushed_edits_and_keeps_source() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from/a.cad"), "a").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        // A single char edit stays batched in a source buffer, not yet in the store.
        guard
            .get_file(&file("from/a.cad"))
            .unwrap()
            .insert_char(crate::kernel::transaction::CursorIndex { line: 0, col: 0 }, 'Z')
            .unwrap();

        memfile_blocking(&mut guard, folder("from"), folder("to")).unwrap();

        // Both subtrees hold the edited text; the source and its buffer stay in place.
        assert_eq!(handle.read_file(&file("to/a.cad")).unwrap(), "Za");
        assert_eq!(handle.read_file(&file("from/a.cad")).unwrap(), "Za");
        assert!(guard.file_map.contains_key(std::path::Path::new("from/a.cad")));
    }
}
