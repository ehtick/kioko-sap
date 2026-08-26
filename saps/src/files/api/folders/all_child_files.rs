use crate::errors::file::FileError;

use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};

/// Lists every file beneath a folder through a blocking IO handle.
///
/// The handle is any type implementing `FolderIo` — a disk, in-memory, or key-value backend
/// — so the same API call works against whichever storage is slotted in. Unlike
/// `child_files`, this descends into every sub-folder and returns the whole subtree.
///
/// # Arguments
/// * `handle`: The blocking IO backend to list through.
/// * `path`: The folder whose subtree is listed.
///
/// # Returns
/// Every descendant file, or a `FileError` if the listing fails.
pub fn blocking<H: FolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FilePath>>, FileError> {
    handle.all_child_files(path)
}

/// Lists every file beneath a folder through an async IO handle.
///
/// The async counterpart of `blocking`. The handle is any type implementing `AsyncFolderIo`,
/// so the same API call works against whichever async storage is slotted in.
///
/// # Arguments
/// * `handle`: The async IO backend to list through.
/// * `path`: The folder whose subtree is listed.
///
/// # Returns
/// Every descendant file, or a `FileError` if the listing fails.
pub async fn asynchronous<H: AsyncFolderIo>(
    handle: &H,
    path: Path<FolderPath>,
) -> Result<Vec<Path<FilePath>>, FileError> {
    handle.all_child_files(path).await
}

#[cfg(test)]
mod tests {
    // See `child_files.rs` for why only the blocking path is covered here.

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
    fn blocking_lists_whole_subtree() {
        let handle = BlockingMemIo::new();
        handle.write_file(&file("src/a.cad"), "a").unwrap();
        handle.write_file(&file("src/nested/deep.cad"), "deep").unwrap();
        handle.write_file(&file("src/nested/inner/deeper.cad"), "deeper").unwrap();

        let listed = raw_paths(blocking(&handle, folder("src")).unwrap());
        assert_eq!(
            listed,
            vec![
                PathBuf::from("src/a.cad"),
                PathBuf::from("src/nested/deep.cad"),
                PathBuf::from("src/nested/inner/deeper.cad"),
            ]
        );
    }
}
