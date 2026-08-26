use std::path::PathBuf;

use wasm_bindgen::JsValue;
use web_sys::IdbTransactionMode;

use crate::files::engines::async_io::indexed_db::{IndexedDbIo, await_request, js_error};
use crate::files::io::async_folder::AsyncFolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a folder the store does not hold.
///
/// The store keeps only files, so a folder "exists" precisely when at least one file is
/// stored beneath it. When no key has the requested prefix there is nothing to act on, so
/// this raises the same shape of error the other backends use for a missing folder.
///
/// # Arguments
/// * `folder`: The folder that was expected to hold files but was empty or absent.
///
/// # Returns
/// A `FileError::Io` naming the missing folder.
fn not_found(folder: &str) -> FileError {
    FileError::Io { path: folder.to_string(), message: "folder not found".into() }
}

/// Wraps a stored key in a typed `Path<FilePath>`.
///
/// Every key was inserted through the file writer, which only accepts paths that already
/// carry an extension, so this conversion never fails in practice. It still returns a
/// `Result` so a future key that breaks that invariant surfaces rather than panics.
///
/// # Arguments
/// * `key`: A path key read out of the store.
///
/// # Returns
/// The key as a `Path<FilePath>`, or a `FileError` if it has no extension.
fn typed_file(key: &PathBuf) -> Result<Path<FilePath>, FileError> {
    Path::<FilePath>::new(key.to_string_lossy().to_string())
}

/// Rewrites `key` so its `from` prefix becomes `to`.
///
/// # Arguments
/// * `key`: The existing key string, known to start with `from`.
/// * `from`: The prefix currently on the key.
/// * `to`: The prefix to put in its place.
///
/// # Returns
/// The key with its prefix swapped.
fn swap_prefix(key: &str, from: &PathBuf, to: &PathBuf) -> PathBuf {
    let key_path = PathBuf::from(key);
    // The caller only passes keys that start with `from`, so the strip cannot fail.
    let relative = key_path.strip_prefix(from).unwrap();
    to.join(relative)
}

impl AsyncFolderIo for IndexedDbIo {
    /// Lists the files stored directly inside `path`, without descending into sub-folders.
    ///
    /// The store is a flat map with no explicit folders, so a file is an immediate child of
    /// `path` when its parent equals `path`. A file nested deeper is left out.
    ///
    /// # Arguments
    /// * `path`: The folder to list.
    ///
    /// # Returns
    /// The immediate child files as `Path<FilePath>`s.
    async fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let mut result = Vec::new();
        for key in self.all_keys().await? {
            let key_path = PathBuf::from(&key);
            if key_path.parent() == Some(folder.as_path()) {
                result.push(typed_file(&key_path)?);
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
    /// Every descendant file as a `Path<FilePath>`.
    async fn all_child_files(
        &self,
        path: Path<FolderPath>,
    ) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let mut result = Vec::new();
        for key in self.all_keys().await? {
            let key_path = PathBuf::from(&key);
            if key_path.starts_with(folder) {
                result.push(typed_file(&key_path)?);
            }
        }
        Ok(result)
    }

    /// Lists the immediate sub-folders inside `path`.
    ///
    /// The store is a flat object store with no directory entities, so there are no folders to
    /// enumerate — this always returns an empty list. A caller that needs the directory
    /// structure must use a disk-backed store.
    ///
    /// # Arguments
    /// * `path`: The folder to list (ignored).
    ///
    /// # Returns
    /// An empty vector.
    async fn child_folders(
        &self,
        _path: Path<FolderPath>,
    ) -> Result<Vec<Path<FolderPath>>, FileError> {
        Ok(Vec::new())
    }

    /// Creates a folder at `path`.
    ///
    /// A no-op: the flat object store has no directory entities, so a folder exists only
    /// implicitly once a file is written beneath it. Nothing is materialised.
    ///
    /// # Arguments
    /// * `path`: The folder to create (ignored).
    ///
    /// # Returns
    /// `Ok(())`.
    async fn create_folder(&self, _path: Path<FolderPath>) -> Result<(), FileError> {
        Ok(())
    }

    /// Reports whether any file is stored beneath `path` (what "folder exists" means for the
    /// flat store). Best-effort: a failing read reads as "does not exist".
    ///
    /// # Arguments
    /// * `path`: The folder to check.
    ///
    /// # Returns
    /// `true` if at least one key has `path` as a leading prefix, otherwise `false`.
    async fn folder_exists(&self, path: &Path<FolderPath>) -> bool {
        let folder: &PathBuf = path.into();
        match self.all_keys().await {
            Ok(keys) => keys.iter().any(|key| PathBuf::from(key).starts_with(folder)),
            Err(_) => false,
        }
    }

    /// Deletes every file stored beneath `path`.
    ///
    /// Since the store has no standalone folders, deleting a folder means removing every key
    /// under its prefix. All the delete requests are issued on one transaction before any is
    /// awaited, so the deletion is atomic. If no file is stored beneath `path` there is
    /// nothing to delete, which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `path`: The folder to delete.
    ///
    /// # Returns
    /// `Ok(())` once every file beneath `path` is gone, or a `FileError` if the folder holds
    /// no files.
    async fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError> {
        let folder: &PathBuf = (&path).into();
        let context = folder.to_string_lossy().to_string();
        let keys: Vec<String> = self
            .all_keys()
            .await?
            .into_iter()
            .filter(|key| PathBuf::from(key).starts_with(folder))
            .collect();
        if keys.is_empty() {
            return Err(not_found(&context));
        }

        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let mut requests = Vec::new();
        for key in &keys {
            requests.push(
                store.delete(&JsValue::from_str(key)).map_err(|error| js_error(&context, error))?,
            );
        }
        for request in &requests {
            await_request(request).await.map_err(|error| js_error(&context, error))?;
        }
        Ok(())
    }

    /// Moves every file beneath `from` so it sits beneath `to` instead.
    ///
    /// Each matching key is written under its swapped prefix and the original deleted, all
    /// on one transaction whose requests are issued before any is awaited. If no file is
    /// stored beneath `from` there is nothing to move, which is a missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to move.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    async fn move_folder(
        &self,
        from: Path<FolderPath>,
        to: Path<FolderPath>,
    ) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let context = from_buf.to_string_lossy().to_string();
        let entries: Vec<(String, String)> = self
            .all_entries()
            .await?
            .into_iter()
            .filter(|(key, _)| PathBuf::from(key).starts_with(from_buf))
            .collect();
        if entries.is_empty() {
            return Err(not_found(&context));
        }

        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let mut requests = Vec::new();
        for (key, value) in &entries {
            let new_key = swap_prefix(key, from_buf, to_buf);
            requests.push(
                store
                    .put_with_key(
                        &JsValue::from_str(value),
                        &JsValue::from_str(&new_key.to_string_lossy()),
                    )
                    .map_err(|error| js_error(&context, error))?,
            );
            requests.push(
                store.delete(&JsValue::from_str(key)).map_err(|error| js_error(&context, error))?,
            );
        }
        for request in &requests {
            await_request(request).await.map_err(|error| js_error(&context, error))?;
        }
        Ok(())
    }

    /// Copies every file beneath `from` so it also sits beneath `to`, keeping the source.
    ///
    /// Each matching key is duplicated under its swapped prefix; the source keys stay in
    /// place. If no file is stored beneath `from` there is nothing to copy, which is a
    /// missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to copy.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    async fn copy_folder(
        &self,
        from: Path<FolderPath>,
        to: Path<FolderPath>,
    ) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let context = from_buf.to_string_lossy().to_string();
        let entries: Vec<(String, String)> = self
            .all_entries()
            .await?
            .into_iter()
            .filter(|(key, _)| PathBuf::from(key).starts_with(from_buf))
            .collect();
        if entries.is_empty() {
            return Err(not_found(&context));
        }

        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let mut requests = Vec::new();
        for (key, value) in &entries {
            let new_key = swap_prefix(key, from_buf, to_buf);
            requests.push(
                store
                    .put_with_key(
                        &JsValue::from_str(value),
                        &JsValue::from_str(&new_key.to_string_lossy()),
                    )
                    .map_err(|error| js_error(&context, error))?,
            );
        }
        for request in &requests {
            await_request(request).await.map_err(|error| js_error(&context, error))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // IndexedDB only exists in a browser, so these run under `wasm-bindgen-test` against a
    // headless browser. Each test uses a distinct database name so they stay isolated.

    use super::*;
    use crate::files::io::async_file::AsyncFileIo;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
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

    #[wasm_bindgen_test]
    async fn child_files_lists_only_immediate_files() {
        let io = IndexedDbIo::new("folders_test_child").await.unwrap();
        io.write_file(&file("src/a.cad"), "a").await.unwrap();
        io.write_file(&file("src/b.cad"), "b").await.unwrap();
        io.write_file(&file("src/nested/deep.cad"), "deep").await.unwrap();

        let listed = raw_paths(io.child_files(folder("src")).await.unwrap());
        assert_eq!(listed, vec![PathBuf::from("src/a.cad"), PathBuf::from("src/b.cad")]);
    }

    #[wasm_bindgen_test]
    async fn all_child_files_lists_whole_subtree() {
        let io = IndexedDbIo::new("folders_test_all").await.unwrap();
        io.write_file(&file("src/a.cad"), "a").await.unwrap();
        io.write_file(&file("src/nested/deep.cad"), "deep").await.unwrap();

        let listed = raw_paths(io.all_child_files(folder("src")).await.unwrap());
        assert_eq!(listed, vec![PathBuf::from("src/a.cad"), PathBuf::from("src/nested/deep.cad")]);
    }

    #[wasm_bindgen_test]
    async fn delete_folder_removes_all_files_beneath() {
        let io = IndexedDbIo::new("folders_test_delete").await.unwrap();
        io.write_file(&file("src/a.cad"), "a").await.unwrap();
        io.write_file(&file("src/nested/b.cad"), "b").await.unwrap();
        io.write_file(&file("other/c.cad"), "c").await.unwrap();

        io.delete_folder(folder("src")).await.unwrap();
        assert!(io.read_file(&file("src/a.cad")).await.is_err());
        assert!(io.read_file(&file("src/nested/b.cad")).await.is_err());
        assert_eq!(io.read_file(&file("other/c.cad")).await.unwrap(), "c");
    }

    #[wasm_bindgen_test]
    async fn delete_folder_missing_folder_errors() {
        let io = IndexedDbIo::new("folders_test_delete_missing").await.unwrap();
        assert!(io.delete_folder(folder("missing")).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn move_folder_moves_subtree_and_removes_source() {
        let io = IndexedDbIo::new("folders_test_move").await.unwrap();
        io.write_file(&file("from/a.cad"), "a").await.unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").await.unwrap();

        io.move_folder(folder("from"), folder("to")).await.unwrap();
        assert_eq!(io.read_file(&file("to/a.cad")).await.unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).await.unwrap(), "b");
        assert!(io.read_file(&file("from/a.cad")).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn copy_folder_duplicates_subtree_and_keeps_source() {
        let io = IndexedDbIo::new("folders_test_copy").await.unwrap();
        io.write_file(&file("from/a.cad"), "a").await.unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").await.unwrap();

        io.copy_folder(folder("from"), folder("to")).await.unwrap();
        assert_eq!(io.read_file(&file("from/a.cad")).await.unwrap(), "a");
        assert_eq!(io.read_file(&file("to/a.cad")).await.unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).await.unwrap(), "b");
    }
}
