use std::path::PathBuf;

use redb::{ReadableDatabase, ReadableTable};

use crate::files::engines::blocking_io::kv::{FILES_TABLE, KvBlockingIo, kv_error};
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a file the database does not hold.
///
/// redb reports a missing key as `Ok(None)` rather than an error, so this raises the
/// equivalent "not found" error itself, matching the shape the disk and memory backends
/// use for the same situation.
///
/// # Arguments
/// * `key`: The path key that was expected in the database but was not present.
///
/// # Returns
/// A `FileError::Io` naming the missing path.
fn not_found(key: &str) -> FileError {
    FileError::Io { path: key.to_string(), message: "file not found".into() }
}

impl FileIo for KvBlockingIo {
    /// Reads the contents stored for `path` in the database.
    ///
    /// Opens a read transaction and looks the path up in the files table. The path is a
    /// `Path<FilePath>`, so the caller has already proven it points at a file.
    ///
    /// # Arguments
    /// * `path`: The file to read.
    ///
    /// # Returns
    /// A copy of the stored contents, or a `FileError` if no file is stored at `path`.
    fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();

        let read_txn = self.db.begin_read().map_err(|error| kv_error(&key, error))?;
        let table = read_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&key, error))?;
        match table.get(key.as_str()).map_err(|error| kv_error(&key, error))? {
            Some(guard) => Ok(guard.value().to_string()),
            None => Err(not_found(&key)),
        }
    }

    /// Reports whether a file is stored at `path`.
    ///
    /// Best-effort: any database error (failing to open the txn/table) reads as "does not
    /// exist", since this is only a conflict pre-check and the real operation still surfaces
    /// its own error.
    ///
    /// # Arguments
    /// * `path`: The file to check.
    ///
    /// # Returns
    /// `true` if the database holds a file at `path`, otherwise `false`.
    fn exists(&self, path: &Path<FilePath>) -> bool {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        let lookup = || -> Result<bool, FileError> {
            let read_txn = self.db.begin_read().map_err(|error| kv_error(&key, error))?;
            let table = read_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&key, error))?;
            Ok(table.get(key.as_str()).map_err(|error| kv_error(&key, error))?.is_some())
        };
        lookup().unwrap_or(false)
    }

    /// Writes contents to `path`, inserting a new entry or replacing an existing one.
    ///
    /// The whole write happens in a single committed transaction. The path is a
    /// `Path<FilePath>`, so the caller has already proven it points at a file.
    ///
    /// # Arguments
    /// * `path`: The file to write.
    /// * `data`: The data to be written to the file.
    ///
    /// # Returns
    /// `Ok(())` once the transaction commits.
    fn write_file<X: Into<String>>(&self, path: &Path<FilePath>, data: X) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();
        let data = data.into();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&key, error))?;
        {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&key, error))?;
            table.insert(key.as_str(), data.as_str()).map_err(|error| kv_error(&key, error))?;
        }
        write_txn.commit().map_err(|error| kv_error(&key, error))?;
        Ok(())
    }

    /// Deletes the entry stored for `path`.
    ///
    /// If no entry exists the transaction is dropped without committing, so the database is
    /// left untouched. The path is a `Path<FilePath>`, so this only ever removes a file.
    ///
    /// # Arguments
    /// * `path`: The file to delete.
    ///
    /// # Returns
    /// `Ok(())` once the entry is gone, or a `FileError` if no file is stored at `path`.
    fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        let key = path_buf.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&key, error))?;
        let existed = {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&key, error))?;
            table.remove(key.as_str()).map_err(|error| kv_error(&key, error))?.is_some()
        };
        if !existed {
            // Drop the transaction without committing so nothing changes.
            return Err(not_found(&key));
        }
        write_txn.commit().map_err(|error| kv_error(&key, error))?;
        Ok(())
    }

    /// Moves the entry from `from` to `to`, removing the source.
    ///
    /// The read, remove and insert all happen in one transaction, so the move is atomic:
    /// either the whole move commits or nothing changes. Both paths are files.
    ///
    /// # Arguments
    /// * `from`: The existing file to move.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if no file is stored at `from`.
    fn move_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        let from_key = from_buf.to_string_lossy().to_string();
        let to_key = to_buf.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&from_key, error))?;
        {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&from_key, error))?;
            let contents =
                match table.get(from_key.as_str()).map_err(|error| kv_error(&from_key, error))? {
                    Some(guard) => guard.value().to_string(),
                    None => return Err(not_found(&from_key)),
                };
            table.remove(from_key.as_str()).map_err(|error| kv_error(&from_key, error))?;
            table
                .insert(to_key.as_str(), contents.as_str())
                .map_err(|error| kv_error(&to_key, error))?;
        }
        write_txn.commit().map_err(|error| kv_error(&from_key, error))?;
        Ok(())
    }

    /// Copies the entry from `from` to `to`, leaving the source in place.
    ///
    /// The read and insert happen in one transaction. Both paths are files.
    ///
    /// # Arguments
    /// * `from`: The existing file to copy.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if no file is stored at `from`.
    fn copy_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        let from_key = from_buf.to_string_lossy().to_string();
        let to_key = to_buf.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&from_key, error))?;
        {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&from_key, error))?;
            let contents =
                match table.get(from_key.as_str()).map_err(|error| kv_error(&from_key, error))? {
                    Some(guard) => guard.value().to_string(),
                    None => return Err(not_found(&from_key)),
                };
            table
                .insert(to_key.as_str(), contents.as_str())
                .map_err(|error| kv_error(&to_key, error))?;
        }
        write_txn.commit().map_err(|error| kv_error(&from_key, error))?;
        Ok(())
    }
}

// Filesystem-backed: these drive a real temp directory, so they are native-only.
// A `--target wasm32-unknown-unknown` test build compiles every test module,
// and `tempfile` has no meaning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    // Each test opens a fresh redb database inside a `tempfile::tempdir()`, so the store is
    // real and persistent but isolated to the test and cleaned up when the temp dir drops.

    use super::*;
    use tempfile::{TempDir, tempdir};

    /// Builds a KV engine over a new database inside `dir`.
    fn kv(dir: &TempDir) -> KvBlockingIo {
        KvBlockingIo::new(dir.path().join("test.redb")).unwrap()
    }

    /// Builds a typed file path from a plain name.
    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn read_file_returns_file_contents() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("main.cad"), "hello world").unwrap();

        assert_eq!(io.read_file(&file("main.cad")).unwrap(), "hello world");
    }

    #[test]
    fn read_file_missing_file_errors() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);

        assert!(io.read_file(&file("missing.cad")).is_err());
    }

    #[test]
    fn write_file_creates_new_file() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        assert!(io.read_file(&file("new.cad")).is_err());

        io.write_file(&file("new.cad"), "created").unwrap();
        assert_eq!(io.read_file(&file("new.cad")).unwrap(), "created");
    }

    #[test]
    fn write_file_overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("existing.cad"), "old contents").unwrap();

        io.write_file(&file("existing.cad"), "new").unwrap();
        // The old contents are fully replaced, not appended to.
        assert_eq!(io.read_file(&file("existing.cad")).unwrap(), "new");
    }

    #[test]
    fn delete_file_removes_file() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("doomed.cad"), "bye").unwrap();

        io.delete_file(&file("doomed.cad")).unwrap();
        assert!(io.read_file(&file("doomed.cad")).is_err());
    }

    #[test]
    fn delete_file_missing_file_errors() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);

        assert!(io.delete_file(&file("missing.cad")).is_err());
    }

    #[test]
    fn move_file_moves_contents_and_removes_source() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("from.cad"), "payload").unwrap();

        io.move_file(&file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(io.read_file(&file("to.cad")).unwrap(), "payload");
        assert!(io.read_file(&file("from.cad")).is_err());
    }

    #[test]
    fn copy_file_duplicates_contents_and_keeps_source() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("from.cad"), "payload").unwrap();

        io.copy_file(&file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(io.read_file(&file("from.cad")).unwrap(), "payload");
        assert_eq!(io.read_file(&file("to.cad")).unwrap(), "payload");
    }

    #[test]
    fn contents_persist_across_reopen() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("persist.redb");
        {
            let io = KvBlockingIo::new(&db_path).unwrap();
            io.write_file(&file("kept.cad"), "durable").unwrap();
        }
        // A fresh engine over the same file sees the previously written data.
        let reopened = KvBlockingIo::new(&db_path).unwrap();
        assert_eq!(reopened.read_file(&file("kept.cad")).unwrap(), "durable");
    }

    #[test]
    fn exists_reports_stored_files() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        assert!(!io.exists(&file("main.cad")));
        io.write_file(&file("main.cad"), "x").unwrap();
        assert!(io.exists(&file("main.cad")));
    }
}
