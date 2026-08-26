use crate::errors::file::FileError;

use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Lists the immediate sub-folders inside a folder through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo`, so the same call works against whichever
/// storage is slotted in. Only a disk backend enumerates real directories; flat key-value
/// stores have no directory entities and return an empty list.
///
/// # Arguments
/// * `handle`: The blocking IO backend to list through.
/// * `path`: The folder whose immediate sub-folders are listed.
///
/// # Returns
/// The immediate child folders, or a `FileError` if the folder cannot be read.
pub fn blocking<H: FolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FolderPath>>, FileError> {
    handle.child_folders(path)
}

/// Lists the immediate sub-folders inside a folder through an async IO handle.
///
/// The async counterpart of `blocking`, generic over `AsyncFolderIo` so it serves the browser
/// IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `handle`: The async IO backend to list through.
/// * `path`: The folder whose immediate sub-folders are listed.
///
/// # Returns
/// The immediate child folders, or a `FileError` if the folder cannot be read.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FolderPath>>, FileError> {
    handle.child_folders(path).await
}

// Filesystem-backed: these drive a real temp directory, so they are native-only.
// A `--target wasm32-unknown-unknown` test build compiles every test module,
// and `tempfile` has no meaning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    // Disk directory semantics are exercised here via a temp dir. See `child_files.rs` for why
    // only the blocking path runs.

    use super::*;
    use crate::files::engines::blocking_io::disk::BlockingDiskIo;
    use crate::files::io::file::FileIo;
    use crate::files::paths::FilePath;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn blocking_lists_immediate_subfolders() {
        let dir = tempdir().unwrap();
        let root = Path::<FolderPath>::new(dir.path().to_string_lossy().to_string()).unwrap();
        // A nested file materialises `sub` on disk; a top-level file must not appear as a folder.
        let nested =
            Path::<FilePath>::new(dir.path().join("sub/a.cad").to_string_lossy().to_string())
                .unwrap();
        BlockingDiskIo.write_file(&nested, "a").unwrap();

        let listed: Vec<PathBuf> = blocking(&BlockingDiskIo, root)
            .unwrap()
            .iter()
            .map(|f| {
                let buf: &PathBuf = f.into();
                buf.clone()
            })
            .collect();
        assert_eq!(listed, vec![dir.path().join("sub")]);
    }
}
