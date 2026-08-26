use crate::errors::file::FileError;

use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Creates a folder (and any missing parents) through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo`, so the same call works against whichever
/// storage is slotted in. On a disk backend this materialises the directory; on a flat
/// key-value store it is a successful no-op, because a folder exists only implicitly once a
/// file is written beneath it.
///
/// # Arguments
/// * `handle`: The blocking IO backend to create through.
/// * `path`: The folder to create.
///
/// # Returns
/// `Ok(())` once the folder exists, or a `FileError` if it could not be created.
pub fn blocking<H: FolderIo>(handle: &H, path: Path<FolderPath>) -> Result<(), FileError> {
    handle.create_folder(path)
}

/// Creates a folder through an async IO handle.
///
/// The async counterpart of `blocking`, generic over `AsyncFolderIo` so it serves the browser
/// IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `handle`: The async IO backend to create through.
/// * `path`: The folder to create.
///
/// # Returns
/// `Ok(())` once the folder exists, or a `FileError` if it could not be created.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<(), FileError> {
    handle.create_folder(path).await
}

// Filesystem-backed: these drive a real temp directory, so they are native-only.
// A `--target wasm32-unknown-unknown` test build compiles every test module,
// and `tempfile` has no meaning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    // Disk directory semantics are exercised here via a temp dir; the flat-store no-op is
    // covered in the engine impls. See `child_files.rs` for why only the blocking path runs.

    use super::*;
    use crate::files::engines::blocking_io::disk::BlockingDiskIo;
    use tempfile::tempdir;

    #[test]
    fn blocking_creates_directory_on_disk() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("fresh/nested");
        let folder = Path::<FolderPath>::new(target.to_string_lossy().to_string()).unwrap();

        blocking(&BlockingDiskIo, folder).unwrap();
        assert!(target.is_dir());
    }
}
