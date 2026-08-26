use std::path::PathBuf;

use crate::files::engines::blocking_io::mem::BlockingMemIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a folder the store does not hold.
///
/// The in-memory store keeps only files, so a folder "exists" precisely when at least one
/// file is stored beneath it. When no file has the requested prefix there is nothing to
/// act on, so this raises the same shape of error the disk backend gets from the OS.
///
/// # Arguments
/// * `path`: The folder that was expected to hold files but was empty or absent.
///
/// # Returns
/// A `FileError::Io` naming the missing folder.
fn not_found(path: &PathBuf) -> FileError {
    FileError::Io { path: path.to_string_lossy().to_string(), message: "folder not found".into() }
}

/// Wraps a stored key in a typed `Path<FilePath>`.
///
/// Every key in the store was inserted through the file writer, which only accepts paths
/// that already carry an extension, so this conversion never fails in practice. It still
/// returns a `Result` so any future key that breaks that invariant surfaces rather than
/// panics.
///
/// # Arguments
/// * `path`: A key from the backing store.
///
/// # Returns
/// The key as a `Path<FilePath>`, or a `FileError` if it has no extension.
fn typed_file(path: &PathBuf) -> Result<Path<FilePath>, FileError> {
    Path::<FilePath>::new(path.to_string_lossy().to_string())
}

impl FolderIo for BlockingMemIo {
    /// Lists the files stored directly inside `path`, without descending into sub-folders.
    ///
    /// The store is a flat map with no explicit folders, so a file is an immediate child
    /// of `path` when its parent equals `path`. A file nested one level deeper has a
    /// different parent and is left out.
    ///
    /// # Arguments
    /// * `path`: The folder to list.
    ///
    /// # Returns
    /// The immediate child files as `Path<FilePath>`s. The order is unspecified because
    /// the backing map is unordered.
    fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let files = self.files.borrow();
        let mut result = Vec::new();
        for key in files.keys() {
            if key.parent() == Some(folder.as_path()) {
                result.push(typed_file(key)?);
            }
        }
        Ok(result)
    }

    /// Lists every file stored anywhere beneath `path`.
    ///
    /// Because folders are implied by key prefixes, this returns every key that has `path`
    /// as a leading path prefix, however deeply nested.
    ///
    /// # Arguments
    /// * `path`: The folder whose subtree is listed.
    ///
    /// # Returns
    /// Every descendant file as a `Path<FilePath>`. The order is unspecified because the
    /// backing map is unordered.
    fn all_child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let files = self.files.borrow();
        let mut result = Vec::new();
        for key in files.keys() {
            if key.starts_with(folder) {
                result.push(typed_file(key)?);
            }
        }
        Ok(result)
    }

    /// Deletes every file stored beneath `path`.
    ///
    /// Since the store has no standalone folders, deleting a folder means removing every
    /// key under its prefix. If no file is stored beneath `path` there is nothing to
    /// delete, which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `path`: The folder to delete.
    ///
    /// # Returns
    /// `Ok(())` once every file beneath `path` is gone, or a `FileError` if the folder
    /// holds no files.
    /// Lists the immediate sub-folders inside `path`.
    ///
    /// The store is a flat map with no directory entities, so there are no folders to
    /// enumerate — this always returns an empty list. A caller that needs the directory
    /// structure must use a disk-backed store.
    ///
    /// # Arguments
    /// * `path`: The folder to list (ignored).
    ///
    /// # Returns
    /// An empty vector.
    fn child_folders(&self, _path: Path<FolderPath>) -> Result<Vec<Path<FolderPath>>, FileError> {
        Ok(Vec::new())
    }

    /// Creates a folder at `path`.
    ///
    /// A no-op: the flat store has no directory entities, so a folder exists only implicitly
    /// once a file is written beneath it. There is nothing to materialise, and no spurious key
    /// is inserted.
    ///
    /// # Arguments
    /// * `path`: The folder to create (ignored).
    ///
    /// # Returns
    /// `Ok(())`.
    fn create_folder(&self, _path: Path<FolderPath>) -> Result<(), FileError> {
        Ok(())
    }

    /// Reports whether any file is stored beneath `path` (which is what "folder exists" means
    /// for the flat store).
    ///
    /// # Arguments
    /// * `path`: The folder to check.
    ///
    /// # Returns
    /// `true` if at least one file has `path` as a leading prefix, otherwise `false`.
    fn folder_exists(&self, path: &Path<FolderPath>) -> bool {
        let folder: &PathBuf = path.into();
        self.files.borrow().keys().any(|key| key.starts_with(folder))
    }

    fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError> {
        let folder: &PathBuf = (&path).into();
        let mut files = self.files.borrow_mut();
        let keys: Vec<PathBuf> =
            files.keys().filter(|key| key.starts_with(folder)).cloned().collect();
        if keys.is_empty() {
            return Err(not_found(folder));
        }
        for key in keys {
            files.remove(&key);
        }
        Ok(())
    }

    /// Moves every file beneath `from` so it sits beneath `to` instead.
    ///
    /// Each matching key has its `from` prefix swapped for `to`, and the original key is
    /// removed. After a successful move nothing remains under `from`. If no file is stored
    /// beneath `from` there is nothing to move, which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to move.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    fn move_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let mut files = self.files.borrow_mut();
        let keys: Vec<PathBuf> =
            files.keys().filter(|key| key.starts_with(from_buf)).cloned().collect();
        if keys.is_empty() {
            return Err(not_found(from_buf));
        }
        for key in keys {
            // The filter guarantees the prefix is present, so the strip cannot fail.
            let relative = key.strip_prefix(from_buf).unwrap();
            let new_key = to_buf.join(relative);
            let contents = files.remove(&key).unwrap();
            files.insert(new_key, contents);
        }
        Ok(())
    }

    /// Copies every file beneath `from` so it also sits beneath `to`, keeping the source.
    ///
    /// Each matching key is duplicated with its `from` prefix swapped for `to`; the
    /// original keys stay in place. If no file is stored beneath `from` there is nothing
    /// to copy, which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to copy.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    fn copy_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let mut files = self.files.borrow_mut();
        let keys: Vec<PathBuf> =
            files.keys().filter(|key| key.starts_with(from_buf)).cloned().collect();
        if keys.is_empty() {
            return Err(not_found(from_buf));
        }
        for key in keys {
            let relative = key.strip_prefix(from_buf).unwrap();
            let new_key = to_buf.join(relative);
            let contents = files.get(&key).cloned().unwrap();
            files.insert(new_key, contents);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The in-memory store needs no filesystem, so each test seeds a fresh `BlockingMemIo`
    // through the file writer and drives the folder operations against it. Effects are
    // checked through the same public interfaces rather than by poking at the map.

    use super::*;
    use crate::files::io::file::FileIo;

    /// Builds a typed folder path from a plain name.
    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    /// Builds a typed file path from a plain name.
    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    /// Collects a listing result into a sorted set of raw paths for order-independent asserts.
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
    fn child_files_lists_only_immediate_files() {
        let io = BlockingMemIo::new();
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/b.cad"), "b").unwrap();
        // A nested file must not appear in the immediate listing.
        io.write_file(&file("src/nested/deep.cad"), "deep").unwrap();

        let listed = raw_paths(io.child_files(folder("src")).unwrap());
        assert_eq!(listed, vec![PathBuf::from("src/a.cad"), PathBuf::from("src/b.cad")]);
    }

    #[test]
    fn all_child_files_lists_whole_subtree() {
        let io = BlockingMemIo::new();
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/nested/deep.cad"), "deep").unwrap();
        io.write_file(&file("src/nested/inner/deeper.cad"), "deeper").unwrap();

        let listed = raw_paths(io.all_child_files(folder("src")).unwrap());
        assert_eq!(
            listed,
            vec![
                PathBuf::from("src/a.cad"),
                PathBuf::from("src/nested/deep.cad"),
                PathBuf::from("src/nested/inner/deeper.cad"),
            ]
        );
    }

    #[test]
    fn delete_folder_removes_all_files_beneath() {
        let io = BlockingMemIo::new();
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/nested/b.cad"), "b").unwrap();
        // A sibling folder must be left untouched.
        io.write_file(&file("other/c.cad"), "c").unwrap();

        io.delete_folder(folder("src")).unwrap();
        assert!(io.read_file(&file("src/a.cad")).is_err());
        assert!(io.read_file(&file("src/nested/b.cad")).is_err());
        assert_eq!(io.read_file(&file("other/c.cad")).unwrap(), "c");
    }

    #[test]
    fn delete_folder_missing_folder_errors() {
        let io = BlockingMemIo::new();

        assert!(io.delete_folder(folder("missing")).is_err());
    }

    #[test]
    fn move_folder_moves_subtree_and_removes_source() {
        let io = BlockingMemIo::new();
        io.write_file(&file("from/a.cad"), "a").unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").unwrap();

        io.move_folder(folder("from"), folder("to")).unwrap();
        assert_eq!(io.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).unwrap(), "b");
        assert!(io.read_file(&file("from/a.cad")).is_err());
        assert!(io.read_file(&file("from/nested/b.cad")).is_err());
    }

    #[test]
    fn move_folder_missing_folder_errors() {
        let io = BlockingMemIo::new();

        assert!(io.move_folder(folder("missing"), folder("to")).is_err());
    }

    #[test]
    fn copy_folder_duplicates_subtree_and_keeps_source() {
        let io = BlockingMemIo::new();
        io.write_file(&file("from/a.cad"), "a").unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").unwrap();

        io.copy_folder(folder("from"), folder("to")).unwrap();
        // Source is untouched.
        assert_eq!(io.read_file(&file("from/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("from/nested/b.cad")).unwrap(), "b");
        // Destination mirrors the whole subtree.
        assert_eq!(io.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).unwrap(), "b");
    }

    #[test]
    fn folder_exists_is_true_when_a_file_is_stored_beneath() {
        let io = BlockingMemIo::new();
        assert!(!io.folder_exists(&folder("src")));
        io.write_file(&file("src/a.cad"), "a").unwrap();
        assert!(io.folder_exists(&folder("src")));
    }

    #[test]
    fn create_folder_and_child_folders_are_flat_no_ops() {
        let io = BlockingMemIo::new();
        // No directory entities: create is a success no-op and inserts no key.
        io.create_folder(folder("empty")).unwrap();
        assert!(io.all_child_files(folder("empty")).unwrap().is_empty());
        // The flat store never enumerates sub-folders.
        io.write_file(&file("src/nested/a.cad"), "a").unwrap();
        assert!(io.child_folders(folder("src")).unwrap().is_empty());
    }
}
