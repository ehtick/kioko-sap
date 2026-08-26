use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Reports whether a file exists through a blocking IO handle.
///
/// The handle is any type implementing `FileIo`, so the same call works against whichever
/// storage is slotted in. Best-effort: a backend error reads as "does not exist".
///
/// # Arguments
/// * `handle`: The blocking IO backend to check through.
/// * `path`: The file to check.
///
/// # Returns
/// `true` if a file exists at `path`, otherwise `false`.
pub fn blocking<H: FileIo>(handle: &H, path: &Path<FilePath>) -> bool {
    handle.exists(path)
}

/// Reports whether a file exists through an async IO handle.
///
/// The async counterpart of `blocking`, generic over `AsyncFileIo` so it serves the browser
/// IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `handle`: The async IO backend to check through.
/// * `path`: The file to check.
///
/// # Returns
/// `true` if a file exists at `path`, otherwise `false`.
pub async fn asynchronous<H: AsyncFileIo>(handle: &H, path: &Path<FilePath>) -> bool {
    handle.exists(path).await
}

/// Reports whether a file exists through a blocking mem-file guard.
///
/// A file exists for a session if it has a live buffer or the store behind the guard holds
/// it. The buffer check comes first because a buffer can exist for a file the store does not
/// hold yet - a `reset_file` that has not flushed. The check never loads a buffer, so the
/// guard is only borrowed, not mutated.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `path`: The file to check.
///
/// # Returns
/// `true` if a buffer or stored file exists at `path`, otherwise `false`.
pub fn memfile_blocking<S: FileIo>(guard: &MemFileGuard<'_, S>, path: &Path<FilePath>) -> bool {
    guard.is_resident(path) || blocking(guard.store(), path)
}

/// Reports whether a file exists through an async mem-file guard.
///
/// The async counterpart of `memfile_blocking`. Generic over `AsyncFileIo`, so it serves the
/// browser IndexedDB backend and any other async store through the one call. The buffer check
/// matters more here than in the blocking case: async buffers never flush on drop, so an
/// unflushed buffer is the only place a fresh file exists at all.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `path`: The file to check.
///
/// # Returns
/// `true` if a buffer or stored file exists at `path`, otherwise `false`.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &AsyncMemFileGuard<'_, S>,
    path: &Path<FilePath>,
) -> bool {
    guard.is_resident(path) || asynchronous(guard.store(), path).await
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
    fn blocking_reports_existence() {
        let handle = BlockingMemIo::new();
        assert!(!blocking(&handle, &file("main.cad")));
        handle.write_file(&file("main.cad"), "x").unwrap();
        assert!(blocking(&handle, &file("main.cad")));
    }

    #[test]
    fn memfile_sees_the_store_through_the_guard() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("main.cad"), "x").unwrap();
        let guard = MemFileGuard::new(&handle);

        // The file was never opened, so it has no buffer - the store answers for it
        assert!(memfile_blocking(&guard, &file("main.cad")));
        assert!(!memfile_blocking(&guard, &file("missing.cad")));
    }

    #[test]
    fn memfile_sees_an_unflushed_buffer() {
        let handle = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&handle);

        // The buffer is seeded without a flush, so the store does not hold the file yet -
        // the live buffer alone makes it exist for the session
        guard.reset_file(&file("fresh.cad"), "x".to_string());

        assert!(!blocking(&handle, &file("fresh.cad")));
        assert!(memfile_blocking(&guard, &file("fresh.cad")));
    }
}
