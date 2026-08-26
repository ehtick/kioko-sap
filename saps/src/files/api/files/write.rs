use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Writes a file through a blocking IO handle.
///
/// The handle is any type implementing `FileIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. An existing file
/// at `path` is overwritten.
///
/// # Arguments
/// * `handle`: The blocking IO backend to write through.
/// * `path`: The file to write.
/// * `data`: The contents to write.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the write fails.
pub fn blocking<H: FileIo, X: Into<String>>(
    handle: &H,
    path: &Path<FilePath>,
    data: X,
) -> Result<(), FileError> {
    handle.write_file(path, data)
}

/// Writes a file through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFileIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to write through.
/// * `path`: The file to write.
/// * `data`: The contents to write.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the write fails.
pub async fn asynchronous<H: AsyncFileIo, X: Into<String>>(
    handle: &H,
    path: &Path<FilePath>,
    data: X,
) -> Result<(), FileError> {
    handle.write_file(path, data).await
}

/// Writes a file through a blocking mem-file guard: a whole-file overwrite that re-seeds the
/// buffer rather than evicting it, then flushes to the store.
///
/// This is the full-overwrite counterpart to `apply_transaction`. The buffer is re-seeded with
/// `data` and kept resident as the session's source of truth — so the next read serves it from
/// memory — and flushed to the store so the durable copy matches. An existing buffer's stale
/// edits are discarded (the overwrite is the new truth).
///
/// # Arguments
/// * `guard`: The blocking guard whose buffer is re-seeded.
/// * `path`: The file to write.
/// * `data`: The contents to write.
///
/// # Returns
/// `Ok(())` once the buffer is re-seeded and flushed, or a `FileError` if the flush fails.
pub fn memfile_blocking<S: FileIo, X: Into<String>>(
    guard: &mut MemFileGuard<'_, S>,
    path: &Path<FilePath>,
    data: X,
) -> Result<(), FileError> {
    guard.reset_file(path, data.into());
    guard.get_file(path)?.save()
}

/// Writes a file through an async mem-file guard, re-seeding the buffer and flushing to the
/// store.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call. The explicit
/// flush matters here because an async buffer does not flush on drop.
///
/// # Arguments
/// * `guard`: The async guard whose buffer is re-seeded.
/// * `path`: The file to write.
/// * `data`: The contents to write.
///
/// # Returns
/// `Ok(())` once the buffer is re-seeded and flushed, or a `FileError` if the flush fails.
pub async fn memfile_asynchronous<S: AsyncFileIo, X: Into<String>>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    path: &Path<FilePath>,
    data: X,
) -> Result<(), FileError> {
    guard.reset_file(path, data.into());
    guard.get_file(path).await?.save().await
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
    fn blocking_writes_through_handle() {
        let handle = BlockingMemIo::new();
        blocking(&handle, &file("main.cad"), "written").unwrap();
        assert_eq!(handle.read_file(&file("main.cad")).unwrap(), "written");
    }

    #[test]
    fn blocking_overwrites_existing() {
        let handle = BlockingMemIo::new();
        blocking(&handle, &file("main.cad"), "old").unwrap();
        blocking(&handle, &file("main.cad"), "new").unwrap();
        assert_eq!(handle.read_file(&file("main.cad")).unwrap(), "new");
    }

    #[test]
    fn memfile_reseeds_buffer_and_flushes_to_store() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("main.cad"), "old").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        memfile_blocking(&mut guard, &file("main.cad"), "overwritten").unwrap();

        // The store has the overwrite, and the buffer stays resident serving the same text.
        assert_eq!(handle.read_file(&file("main.cad")).unwrap(), "overwritten");
        assert_eq!(guard.get_file(&file("main.cad")).unwrap().contents(), "overwritten");
    }
}
