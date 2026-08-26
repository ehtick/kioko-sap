use std::path::PathBuf;

use wasm_bindgen::JsValue;
use web_sys::IdbTransactionMode;

use crate::files::engines::async_io::indexed_db::{IndexedDbIo, await_request, js_error};
use crate::files::io::async_file::AsyncFileIo;
use crate::files::paths::{FilePath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a file the store does not hold.
///
/// IndexedDB reports a missing key as an `undefined` result rather than an error, so this
/// raises the equivalent "not found" error itself, matching the shape the other backends
/// use for the same situation.
///
/// # Arguments
/// * `key`: The path key that was expected in the store but was not present.
///
/// # Returns
/// A `FileError::Io` naming the missing path.
fn not_found(key: &str) -> FileError {
    FileError::Io { path: key.to_string(), message: "file not found".into() }
}

/// Reads the value stored at `key`, returning `None` when the store holds no such key.
///
/// # Arguments
/// * `io`: The engine to read from.
/// * `key`: The path key to look up.
///
/// # Returns
/// `Some(contents)` if the key exists, `None` if it does not, or a `FileError` if the read
/// itself fails.
async fn get(io: &IndexedDbIo, key: &str) -> Result<Option<String>, FileError> {
    let store = io.object_store(IdbTransactionMode::Readonly)?;
    let request = store.get(&JsValue::from_str(key)).map_err(|error| js_error(key, error))?;
    let result = await_request(&request).await.map_err(|error| js_error(key, error))?;
    if result.is_undefined() || result.is_null() {
        return Ok(None);
    }
    Ok(result.as_string())
}

impl AsyncFileIo for IndexedDbIo {
    /// Reads the contents stored for `path` in the database.
    ///
    /// The path is a `Path<FilePath>`, so the caller has already proven it points at a file.
    ///
    /// # Arguments
    /// * `path`: The file to read.
    ///
    /// # Returns
    /// A copy of the stored contents, or a `FileError` if no file is stored at `path`.
    async fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        get(self, &key).await?.ok_or_else(|| not_found(&key))
    }

    /// Reports whether a file is stored at `path`.
    ///
    /// Best-effort: a failing IndexedDB read reads as "does not exist", since this is only a
    /// conflict pre-check and the real operation still surfaces its own error.
    ///
    /// # Arguments
    /// * `path`: The file to check.
    ///
    /// # Returns
    /// `true` if the store holds a value at `path`, otherwise `false`.
    async fn exists(&self, path: &Path<FilePath>) -> bool {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        get(self, &key).await.map(|value| value.is_some()).unwrap_or(false)
    }

    /// Writes contents to `path`, inserting a new entry or replacing an existing one.
    ///
    /// The path is a `Path<FilePath>`, so the caller has already proven it points at a file.
    ///
    /// # Arguments
    /// * `path`: The file to write.
    /// * `data`: The data to be written to the file.
    ///
    /// # Returns
    /// `Ok(())` once the write completes.
    async fn write_file<X: Into<String>>(
        &self,
        path: &Path<FilePath>,
        data: X,
    ) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        let data = data.into();
        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let request = store
            .put_with_key(&JsValue::from_str(&data), &JsValue::from_str(&key))
            .map_err(|error| js_error(&key, error))?;
        await_request(&request).await.map_err(|error| js_error(&key, error))?;
        Ok(())
    }

    /// Deletes the entry stored for `path`.
    ///
    /// The entry is looked up first so a missing file is reported as an error rather than
    /// silently succeeding. The path is a `Path<FilePath>`, so this only ever removes a file.
    ///
    /// # Arguments
    /// * `path`: The file to delete.
    ///
    /// # Returns
    /// `Ok(())` once the entry is gone, or a `FileError` if no file is stored at `path`.
    async fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        if get(self, &key).await?.is_none() {
            return Err(not_found(&key));
        }
        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let request =
            store.delete(&JsValue::from_str(&key)).map_err(|error| js_error(&key, error))?;
        await_request(&request).await.map_err(|error| js_error(&key, error))?;
        Ok(())
    }

    /// Moves the entry from `from` to `to`, removing the source.
    ///
    /// The put and delete are issued on one read-write transaction before either is awaited,
    /// so the move is atomic. Both paths are files.
    ///
    /// # Arguments
    /// * `from`: The existing file to move.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if no file is stored at `from`.
    async fn move_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        let from_key = from_buf.to_string_lossy().to_string();
        let to_key = to_buf.to_string_lossy().to_string();

        let contents = get(self, &from_key).await?.ok_or_else(|| not_found(&from_key))?;

        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let put = store
            .put_with_key(&JsValue::from_str(&contents), &JsValue::from_str(&to_key))
            .map_err(|error| js_error(&to_key, error))?;
        let delete = store
            .delete(&JsValue::from_str(&from_key))
            .map_err(|error| js_error(&from_key, error))?;
        await_request(&put).await.map_err(|error| js_error(&to_key, error))?;
        await_request(&delete).await.map_err(|error| js_error(&from_key, error))?;
        Ok(())
    }

    /// Copies the entry from `from` to `to`, leaving the source in place.
    ///
    /// Both paths are files.
    ///
    /// # Arguments
    /// * `from`: The existing file to copy.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if no file is stored at `from`.
    async fn copy_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        let from_key = from_buf.to_string_lossy().to_string();
        let to_key = to_buf.to_string_lossy().to_string();

        let contents = get(self, &from_key).await?.ok_or_else(|| not_found(&from_key))?;

        let store = self.object_store(IdbTransactionMode::Readwrite)?;
        let request = store
            .put_with_key(&JsValue::from_str(&contents), &JsValue::from_str(&to_key))
            .map_err(|error| js_error(&to_key, error))?;
        await_request(&request).await.map_err(|error| js_error(&to_key, error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // IndexedDB only exists in a browser, so these run under `wasm-bindgen-test` against a
    // headless browser. Each test uses a distinct database name so they stay isolated.

    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[wasm_bindgen_test]
    async fn write_then_read_round_trips() {
        let io = IndexedDbIo::new("files_test_read").await.unwrap();
        io.write_file(&file("main.cad"), "hello world").await.unwrap();
        assert_eq!(io.read_file(&file("main.cad")).await.unwrap(), "hello world");
    }

    #[wasm_bindgen_test]
    async fn read_missing_file_errors() {
        let io = IndexedDbIo::new("files_test_missing").await.unwrap();
        assert!(io.read_file(&file("missing.cad")).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn write_overwrites_existing() {
        let io = IndexedDbIo::new("files_test_overwrite").await.unwrap();
        io.write_file(&file("a.cad"), "old").await.unwrap();
        io.write_file(&file("a.cad"), "new").await.unwrap();
        assert_eq!(io.read_file(&file("a.cad")).await.unwrap(), "new");
    }

    #[wasm_bindgen_test]
    async fn delete_removes_file() {
        let io = IndexedDbIo::new("files_test_delete").await.unwrap();
        io.write_file(&file("a.cad"), "bye").await.unwrap();
        io.delete_file(&file("a.cad")).await.unwrap();
        assert!(io.read_file(&file("a.cad")).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn move_moves_contents_and_removes_source() {
        let io = IndexedDbIo::new("files_test_move").await.unwrap();
        io.write_file(&file("from.cad"), "payload").await.unwrap();
        io.move_file(&file("from.cad"), &file("to.cad")).await.unwrap();
        assert_eq!(io.read_file(&file("to.cad")).await.unwrap(), "payload");
        assert!(io.read_file(&file("from.cad")).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn copy_duplicates_contents_and_keeps_source() {
        let io = IndexedDbIo::new("files_test_copy").await.unwrap();
        io.write_file(&file("from.cad"), "payload").await.unwrap();
        io.copy_file(&file("from.cad"), &file("to.cad")).await.unwrap();
        assert_eq!(io.read_file(&file("from.cad")).await.unwrap(), "payload");
        assert_eq!(io.read_file(&file("to.cad")).await.unwrap(), "payload");
    }
}
