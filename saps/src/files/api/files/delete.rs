use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Deletes a file through a blocking IO handle.
///
/// The handle is any type implementing `FileIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in.
///
/// # Arguments
/// * `handle`: The blocking IO backend to delete through.
/// * `path`: The file to delete.
///
/// # Returns
/// `Ok(())` once the file is gone, or a `FileError` if it does not exist or cannot be removed.
pub fn blocking<H: FileIo>(handle: &H, path: &Path<FilePath>) -> Result<(), FileError> {
    handle.delete_file(path)
}

/// Deletes a file through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFileIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to delete through.
/// * `path`: The file to delete.
///
/// # Returns
/// `Ok(())` once the file is gone, or a `FileError` if it does not exist or cannot be removed.
pub async fn asynchronous<H: AsyncFileIo>(
    handle: &H,
    path: &Path<FilePath>,
) -> Result<(), FileError> {
    handle.delete_file(path).await
}

/// Deletes a file through a blocking mem-file guard, evicting the buffer and removing the
/// durable copy.
///
/// The buffer is dropped first, then the file is deleted from the store. Order matters: the
/// buffer's drop-flush writes it back to the store, so it must happen before the store delete —
/// otherwise a flush after the delete would resurrect the file. A path with no open buffer is
/// still deleted from the store.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `path`: The file to delete.
///
/// # Returns
/// `Ok(())` once the buffer is evicted and the store entry removed, or a `FileError` if the
/// store delete fails.
pub fn memfile_blocking<S: FileIo>(
    guard: &mut MemFileGuard<'_, S>,
    path: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_file(path);
    store.delete_file(path)
}

/// Deletes a file through an async mem-file guard, evicting the buffer and removing the durable
/// copy.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `path`: The file to delete.
///
/// # Returns
/// `Ok(())` once the buffer is evicted and the store entry removed, or a `FileError` if the
/// store delete fails.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    path: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.drop_file(path).await;
    store.delete_file(path).await
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
    fn blocking_deletes_through_handle() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("doomed.cad"), "bye").unwrap();
        blocking(&handle, &file("doomed.cad")).unwrap();
        assert!(handle.read_file(&file("doomed.cad")).is_err());
    }

    #[test]
    fn blocking_missing_file_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, &file("missing.cad")).is_err());
    }

    #[test]
    fn memfile_evicts_buffer_and_deletes_store_entry() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("doomed.cad"), "bye").unwrap();
        let mut guard = MemFileGuard::new(&handle);
        // Make it resident so the delete has a buffer to evict.
        guard.get_file(&file("doomed.cad")).unwrap();

        memfile_blocking(&mut guard, &file("doomed.cad")).unwrap();

        // Gone from the store, and no buffer left to resurrect it on a later flush.
        assert!(handle.read_file(&file("doomed.cad")).is_err());
        assert!(!guard.file_map.contains_key(std::path::Path::new("doomed.cad")));
    }
}
