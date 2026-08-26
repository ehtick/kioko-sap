use std::fs;
use std::path::PathBuf;

use crate::files::engines::blocking_io::disk::BlockingDiskIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};
use crate::errors::file::FileError;

/// Wraps a `std::io::Error` into a `FileError::Io`, tagging it with the path it happened on.
///
/// Every disk call funnels its failures through here so the caller gets a consistent
/// `FileError` carrying both the path that failed and the underlying OS message.
///
/// # Arguments
/// * `path`: The file the failing operation was acting on.
/// * `error`: The underlying `std::io::Error` returned by the filesystem call.
///
/// # Returns
/// A `FileError::Io` holding the path and the OS error message.
fn io_error(path: &PathBuf, error: std::io::Error) -> FileError {
    FileError::Io { path: path.to_string_lossy().to_string(), message: error.to_string() }
}

impl FileIo for BlockingDiskIo {
    /// Reads the entire file at `path` into a `String`.
    ///
    /// The whole file is loaded into memory in one blocking call. The path is a
    /// `Path<FilePath>`, so the caller has already proven it points at a file rather
    /// than a folder.
    ///
    /// # Arguments
    /// * `path`: The file to read.
    ///
    /// # Returns
    /// The file's contents as a `String`, or a `FileError` if the file is missing,
    /// unreadable, or not valid UTF-8.
    fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError> {
        let path_buf: &PathBuf = path.into();
        fs::read_to_string(path_buf).map_err(|error| io_error(path_buf, error))
    }

    /// Reports whether a file exists on disk at `path`.
    ///
    /// True only for a regular file — a directory at `path` reads as `false`, so a caller can
    /// tell a file from a folder by pairing this with the folder backend's `exists`.
    ///
    /// # Arguments
    /// * `path`: The file to check.
    ///
    /// # Returns
    /// `true` if a regular file exists at `path`, otherwise `false`.
    fn exists(&self, path: &Path<FilePath>) -> bool {
        let path_buf: &PathBuf = path.into();
        path_buf.is_file()
    }

    /// Writes contents to the file at `path`, creating it and any missing parent folders.
    ///
    /// An existing file is overwritten. Missing parent directories are created first, so
    /// writing `a/b/c.cad` into a fresh tree succeeds without a separate create-folder step.
    /// The path is a `Path<FilePath>`, so the caller has already proven it points at a file.
    ///
    /// # Arguments
    /// * `path`: The file to write.
    /// * `data`: The data to be written to the file
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if the parents or the file could not be written.
    fn write_file<X: Into<String>>(&self, path: &Path<FilePath>, data: X) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        if let Some(parent) = path_buf.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(&parent.to_path_buf(), error))?;
        }
        fs::write(path_buf, data.into()).map_err(|error| io_error(path_buf, error))
    }

    /// Deletes the file at `path`.
    ///
    /// The path is a `Path<FilePath>`, so this only ever removes a file, never a folder.
    ///
    /// # Arguments
    /// * `path`: The file to delete.
    ///
    /// # Returns
    /// `Ok(())` once the file is gone, or a `FileError` if it does not exist or could
    /// not be removed.
    fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        fs::remove_file(path_buf).map_err(|error| io_error(path_buf, error))
    }

    /// Moves the file from `from` to `to`, renaming it in the process.
    ///
    /// After a successful move the source no longer exists and its contents live at the
    /// destination. Both paths are files, so a folder can never be moved by mistake.
    ///
    /// # Arguments
    /// * `from`: The existing file to move.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if the source is missing or the destination
    /// could not be written.
    fn move_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        fs::rename(from_buf, to_buf).map_err(|error| io_error(from_buf, error))
    }

    /// Copies the file from `from` to `to`, leaving the source in place.
    ///
    /// After a successful copy both the source and the destination exist and hold the
    /// same contents. Both paths are files, so a folder can never be copied by mistake.
    ///
    /// # Arguments
    /// * `from`: The existing file to copy.
    /// * `to`: The destination file path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if the source is missing or the destination
    /// could not be written.
    fn copy_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = from.into();
        let to_buf: &PathBuf = to.into();
        fs::copy(from_buf, to_buf).map(|_| ()).map_err(|error| io_error(from_buf, error))
    }
}

// Filesystem-backed: these drive a real temp directory, so they are native-only.
// A `--target wasm32-unknown-unknown` test build compiles every test module,
// and `tempfile` has no meaning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    // Each test runs against a fresh `tempfile::tempdir()` so the real filesystem is
    // exercised without touching the project tree. The temp dir cleans itself up when
    // the `TempDir` guard is dropped at the end of the test.

    use super::*;
    use tempfile::{TempDir, tempdir};

    /// Builds a typed file path inside `dir` and returns it alongside the raw `PathBuf`,
    /// so a test can both feed the `Path` to the IO under test and check the file on
    /// disk directly.
    fn file_in(dir: &TempDir, name: &str) -> (Path<FilePath>, PathBuf) {
        let raw = dir.path().join(name);
        let typed = Path::<FilePath>::new(raw.to_string_lossy().to_string()).unwrap();
        (typed, raw)
    }

    #[test]
    fn read_file_returns_file_contents() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "main.cad");
        fs::write(&raw, "hello world").unwrap();

        let contents = BlockingDiskIo.read_file(&path).unwrap();
        assert_eq!(contents, "hello world");
    }

    #[test]
    fn read_file_missing_file_errors() {
        let dir = tempdir().unwrap();
        let (path, _raw) = file_in(&dir, "missing.cad");

        assert!(BlockingDiskIo.read_file(&path).is_err());
    }

    #[test]
    fn write_file_creates_new_file() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "new.cad");
        assert!(!raw.exists());

        BlockingDiskIo.write_file(&path, "created").unwrap();
        assert_eq!(fs::read_to_string(&raw).unwrap(), "created");
    }

    #[test]
    fn write_file_overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "existing.cad");
        fs::write(&raw, "old contents").unwrap();

        BlockingDiskIo.write_file(&path, "new").unwrap();
        // The old contents are fully replaced, not appended to.
        assert_eq!(fs::read_to_string(&raw).unwrap(), "new");
    }

    #[test]
    fn delete_file_removes_file() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "doomed.cad");
        fs::write(&raw, "bye").unwrap();
        assert!(raw.exists());

        BlockingDiskIo.delete_file(&path).unwrap();
        assert!(!raw.exists());
    }

    #[test]
    fn delete_file_missing_file_errors() {
        let dir = tempdir().unwrap();
        let (path, _raw) = file_in(&dir, "missing.cad");

        assert!(BlockingDiskIo.delete_file(&path).is_err());
    }

    #[test]
    fn move_file_moves_contents_and_removes_source() {
        let dir = tempdir().unwrap();
        let (from, from_raw) = file_in(&dir, "from.cad");
        let (to, to_raw) = file_in(&dir, "to.cad");
        fs::write(&from_raw, "payload").unwrap();

        BlockingDiskIo.move_file(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(&to_raw).unwrap(), "payload");
        assert!(!from_raw.exists());
    }

    #[test]
    fn copy_file_duplicates_contents_and_keeps_source() {
        let dir = tempdir().unwrap();
        let (from, from_raw) = file_in(&dir, "from.cad");
        let (to, to_raw) = file_in(&dir, "to.cad");
        fs::write(&from_raw, "payload").unwrap();

        BlockingDiskIo.copy_file(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(&from_raw).unwrap(), "payload");
        assert_eq!(fs::read_to_string(&to_raw).unwrap(), "payload");
    }

    #[test]
    fn write_file_creates_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "new_sub/deeper/file.cad");
        assert!(!raw.parent().unwrap().exists());

        BlockingDiskIo.write_file(&path, "created").unwrap();
        assert_eq!(fs::read_to_string(&raw).unwrap(), "created");
    }

    #[test]
    fn exists_reports_only_regular_files() {
        let dir = tempdir().unwrap();
        let (path, raw) = file_in(&dir, "there.cad");
        assert!(!BlockingDiskIo.exists(&path));
        fs::write(&raw, "x").unwrap();
        assert!(BlockingDiskIo.exists(&path));

        // A directory at the path is not a file.
        let (dir_as_file, dir_raw) = file_in(&dir, "adir.cad");
        fs::create_dir(&dir_raw).unwrap();
        assert!(!BlockingDiskIo.exists(&dir_as_file));
    }
}
