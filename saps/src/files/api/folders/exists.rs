use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FolderPath, Path};

/// Reports whether a folder exists through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo`, so the same call works against whichever
/// storage is slotted in. Best-effort: a backend error reads as "does not exist". On a flat
/// key-value store a folder "exists" when at least one file is stored beneath it.
///
/// # Arguments
/// * `handle`: The blocking IO backend to check through.
/// * `path`: The folder to check.
///
/// # Returns
/// `true` if a folder exists at `path`, otherwise `false`.
pub fn blocking<H: FolderIo>(handle: &H, path: &Path<FolderPath>) -> bool {
    handle.folder_exists(path)
}

/// Reports whether a folder exists through an async IO handle.
///
/// The async counterpart of `blocking`, generic over `AsyncFolderIo` so it serves the browser
/// IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `handle`: The async IO backend to check through.
/// * `path`: The folder to check.
///
/// # Returns
/// `true` if a folder exists at `path`, otherwise `false`.
pub async fn asynchronous<H: AsyncFolderIo>(handle: &H, path: &Path<FolderPath>) -> bool {
    handle.folder_exists(path).await
}

#[cfg(test)]
mod tests {
    // See `child_files.rs` for why only the blocking path is covered here.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;
    use crate::files::io::file::FileIo;
    use crate::files::paths::FilePath;

    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn blocking_reports_existence() {
        let handle = BlockingMemIo::new();
        assert!(!blocking(&handle, &folder("src")));
        handle.write_file(&file("src/a.cad"), "a").unwrap();
        assert!(blocking(&handle, &folder("src")));
    }
}
