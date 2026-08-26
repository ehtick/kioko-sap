use crate::errors::file::FileError;

use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};

/// Lists the immediate child files of a folder through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. Only files directly
/// inside the folder are returned; nested files are not.
///
/// # Arguments
/// * `handle`: The blocking IO backend to list through.
/// * `path`: The folder to list.
///
/// # Returns
/// The immediate child files, or a `FileError` if the listing fails.
pub fn blocking<H: FolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FilePath>>, FileError> {
    handle.child_files(path)
}

/// Lists the immediate child files of a folder through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFolderIo`
/// (for example the browser IndexedDB backend), so the same API call works against whichever
/// async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to list through.
/// * `path`: The folder to list.
///
/// # Returns
/// The immediate child files, or a `FileError` if the listing fails.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FilePath>>, FileError> {
    handle.child_files(path).await
}

#[cfg(test)]
mod tests {
    // The async path is generic over `AsyncFolderIo`, whose only backend is browser-only, so
    // it is exercised by the IndexedDB wasm tests rather than here. These host tests cover
    // the blocking path through the in-memory handle.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;
    use crate::files::io::file::FileIo;
    use std::path::PathBuf;

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    fn raw_paths(files: Vec<Path<FilePath>>) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = files
            .iter()
            .map(|file| {
                let buf: &PathBuf = file.into();
                buf.clone()
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn blocking_lists_immediate_children() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("src/a.cad"), "a").unwrap();
        handle.write_file(&file("src/b.cad"), "b").unwrap();
        handle.write_file(&file("src/nested/deep.cad"), "deep").unwrap();

        let listed = raw_paths(blocking(&handle, folder("src")).unwrap());
        assert_eq!(listed, vec![PathBuf::from("src/a.cad"), PathBuf::from("src/b.cad")]);
    }
}
