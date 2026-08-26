use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::file::FileIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Deletes a folder and everything inside it through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. The removal is
/// recursive: all nested files and folders go with it.
///
/// # Arguments
/// * `handle`: The blocking IO backend to delete through.
/// * `path`: The folder to delete.
///
/// # Returns
/// `Ok(())` once the folder is gone, or a `FileError` if it does not exist or cannot be removed.
pub fn blocking<H: FolderIo>(handle: &H, path: Path<FolderPath>) -> Result<(), FileError> {
    handle.delete_folder(path)
}

/// Deletes a folder and everything inside it through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFolderIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to delete through.
/// * `path`: The folder to delete.
///
/// # Returns
/// `Ok(())` once the folder is gone, or a `FileError` if it does not exist or cannot be removed.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<(), FileError> {
    handle.delete_folder(path).await
}

/// Deletes a folder through a blocking mem-file guard, evicting every buffer under it and
/// removing the durable subtree.
///
/// The store must be both a file and a folder backend (`S: FileIo + FolderIo`), which the
/// concrete backends are, so the one store reached through the guard covers both the buffer
/// eviction and the folder removal. Every buffer at or under the folder is dropped first — a
/// directory delete must take its children's buffers with it, or a later flush would resurrect
/// the removed subtree — then the store removes the folder.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `path`: The folder to delete.
///
/// # Returns
/// `Ok(())` once the buffers are evicted and the subtree removed, or a `FileError` if the store
/// delete fails.
pub fn memfile_blocking<S: FileIo + FolderIo>(
    guard: &mut MemFileGuard<'_, S>,
    path: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_dir(&path);
    store.delete_folder(path)
}

/// Deletes a folder through an async mem-file guard, evicting every buffer under it and removing
/// the durable subtree.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo + AsyncFolderIo`, so
/// it serves the browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `path`: The folder to delete.
///
/// # Returns
/// `Ok(())` once the buffers are evicted and the subtree removed, or a `FileError` if the store
/// delete fails.
pub async fn memfile_asynchronous<S: AsyncFileIo + AsyncFolderIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    path: Path<FolderPath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_dir(&path).await;
    store.delete_folder(path).await
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
    fn blocking_deletes_folder_subtree() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("src/a.cad"), "a").unwrap();
        handle.write_file(&file("src/nested/b.cad"), "b").unwrap();

        blocking(&handle, folder("src")).unwrap();
        assert!(handle.read_file(&file("src/a.cad")).is_err());
        assert!(handle.read_file(&file("src/nested/b.cad")).is_err());
    }

    #[test]
    fn blocking_missing_folder_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, folder("missing")).is_err());
    }

    #[test]
    fn memfile_evicts_subtree_buffers_and_deletes_folder() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("src/a.cad"), "a").unwrap();
        handle.write_file(&file("src/nested/b.cad"), "b").unwrap();
        let mut guard = MemFileGuard::new(&handle);
        // Make the subtree resident so the delete has buffers to evict.
        guard.get_file(&file("src/a.cad")).unwrap();
        guard.get_file(&file("src/nested/b.cad")).unwrap();

        memfile_blocking(&mut guard, folder("src")).unwrap();

        // The subtree is gone from the store and no buffers linger to resurrect it.
        assert!(handle.read_file(&file("src/a.cad")).is_err());
        assert!(handle.read_file(&file("src/nested/b.cad")).is_err());
        assert!(guard.file_map.is_empty());
    }
}
