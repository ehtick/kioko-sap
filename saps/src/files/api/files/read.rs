use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Reads a file through a blocking IO handle.
///
/// The handle is any type implementing `FileIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. The call simply
/// forwards to the handle; it exists so callers depend on this stable API surface rather
/// than on a concrete backend.
///
/// # Arguments
/// * `handle`: The blocking IO backend to read through.
/// * `path`: The file to read.
///
/// # Returns
/// The file's contents, or a `FileError` if the read fails.
pub fn blocking<H: FileIo>(handle: &H, path: &Path<FilePath>) -> Result<String, FileError> {
    handle.read_file(path)
}

/// Reads a file through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFileIo`
/// (for example the browser IndexedDB backend), so the same API call works against whichever
/// async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to read through.
/// * `path`: The file to read.
///
/// # Returns
/// The file's contents, or a `FileError` if the read fails.
pub async fn asynchronous<H: AsyncFileIo>(
    handle: &H,
    path: &Path<FilePath>,
) -> Result<String, FileError> {
    handle.read_file(path).await
}

/// Reads a file through a blocking mem-file guard, serving the live buffer.
///
/// The buffer is the source of truth: it carries edits applied but not yet flushed to the
/// store, so a read must come from it rather than the store behind it. The guard loads the
/// file on first use, so a not-yet-resident file is read from the store and kept.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `path`: The file to read.
///
/// # Returns
/// The buffer's current contents, or a `FileError` if the file cannot be loaded.
pub fn memfile_blocking<S: FileIo>(
    guard: &mut MemFileGuard<'_, S>,
    path: &Path<FilePath>,
) -> Result<String, FileError> {
    Ok(guard.get_file(path)?.contents())
}

/// Reads a file through an async mem-file guard, serving the live buffer.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `path`: The file to read.
///
/// # Returns
/// The buffer's current contents, or a `FileError` if the file cannot be loaded.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    path: &Path<FilePath>,
) -> Result<String, FileError> {
    Ok(guard.get_file(path).await?.contents())
}

#[cfg(test)]
mod tests {
    // The async path is generic over `AsyncFileIo`, whose only backend is browser-only, so
    // it is exercised by the IndexedDB wasm tests rather than here. These host tests cover
    // the blocking path through the in-memory handle.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn blocking_reads_through_handle() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("main.cad"), "hello").unwrap();
        assert_eq!(blocking(&handle, &file("main.cad")).unwrap(), "hello");
    }

    #[test]
    fn blocking_missing_file_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, &file("missing.cad")).is_err());
    }

    #[test]
    fn memfile_reads_live_buffer_including_unflushed_edits() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("main.cad"), "hello").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        // A single char edit stays batched in the buffer below the save threshold, so it is
        // not in the store yet — the read must still see it.
        guard.get_file(&file("main.cad")).unwrap().insert_char(cursor(0, 0), 'X').unwrap();

        assert_eq!(memfile_blocking(&mut guard, &file("main.cad")).unwrap(), "Xhello");
    }

    fn cursor(line: usize, col: usize) -> crate::kernel::transaction::CursorIndex {
        crate::kernel::transaction::CursorIndex { line, col }
    }
}
