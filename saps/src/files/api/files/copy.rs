use crate::errors::file::FileError;

use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Copies a file through a blocking IO handle.
///
/// The handle is any type implementing `FileIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. After a successful
/// copy both the source and the destination exist and hold the same contents.
///
/// # Arguments
/// * `handle`: The blocking IO backend to copy through.
/// * `from`: The existing file to copy.
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
    handle.copy_file(from, to)
}

/// Copies a file through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFileIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to copy through.
/// * `from`: The existing file to copy.
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
    handle.copy_file(from, to).await
}

/// Copies a file through a blocking mem-file guard, copying its current text including
/// unflushed edits.
///
/// The source buffer is flushed to the store first — so the copy sees its latest text, not a
/// stale durable copy — while staying resident, since a copy leaves the source in place. Any
/// stale buffer at the destination is evicted so it cannot serve the pre-copy content, then the
/// store copy runs.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `from`: The existing file to copy.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source cannot be loaded or the store copy fails.
pub fn memfile_blocking<S: FileIo>(
    guard: &mut MemFileGuard<'_, S>,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.get_file(from)?.save()?;
    guard.drop_file(to);
    store.copy_file(from, to)
}

/// Copies a file through an async mem-file guard, copying its current text including unflushed
/// edits.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `from`: The existing file to copy.
/// * `to`: The destination file path.
///
/// # Returns
/// `Ok(())` on success, or a `FileError` if the source cannot be loaded or the store copy fails.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &mut AsyncMemFileGuard<'_, S>,
    from: &Path<FilePath>,
    to: &Path<FilePath>,
) -> Result<(), FileError> {
    let store = guard.store();
    guard.get_file(from).await?.save().await?;
    guard.drop_file(to).await;
    store.copy_file(from, to).await
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
    fn blocking_copies_through_handle() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from.cad"), "payload").unwrap();
        blocking(&handle, &file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(handle.read_file(&file("from.cad")).unwrap(), "payload");
        assert_eq!(handle.read_file(&file("to.cad")).unwrap(), "payload");
    }

    #[test]
    fn blocking_missing_source_errors() {
        let handle = BlockingMemIo::new();
        assert!(blocking(&handle, &file("missing.cad"), &file("to.cad")).is_err());
    }

    #[test]
    fn memfile_copy_includes_unflushed_source_edits_and_keeps_source() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("from.cad"), "payload").unwrap();
        let mut guard = MemFileGuard::new(&handle);

        // A single char edit stays batched in the source buffer, not yet in the store.
        guard
            .get_file(&file("from.cad"))
            .unwrap()
            .insert_char(crate::kernel::transaction::CursorIndex { line: 0, col: 0 }, 'Z')
            .unwrap();

        memfile_blocking(&mut guard, &file("from.cad"), &file("to.cad")).unwrap();

        // Both paths hold the edited text, and the source buffer stays resident.
        assert_eq!(handle.read_file(&file("to.cad")).unwrap(), "Zpayload");
        assert_eq!(handle.read_file(&file("from.cad")).unwrap(), "Zpayload");
        assert!(guard.file_map.contains_key(std::path::Path::new("from.cad")));
    }
}
