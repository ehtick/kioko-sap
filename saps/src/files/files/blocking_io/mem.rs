use crate::files::engines::blocking_io::mem::BlockingMemIo;
use std::path::PathBuf;

use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a file the store does not hold.
///
/// The disk backend gets its "missing file" error from the OS; the in-memory store has
/// no OS to ask, so it raises the equivalent error itself when a lookup misses. This
/// keeps both backends returning the same shape of error for the same situation.
///
/// # Arguments
/// * `path`: The file that was expected in the store but was not present.
///
/// # Returns
/// A `FileError::Io` naming the missing path.
fn not_found(path: &PathBuf) -> FileError {
    FileError::Io { path: path.to_string_lossy().to_string(), message: "file not found".into() }
}

impl FileIo for BlockingMemIo {
    /// Reads the contents stored for `path`.
    ///
    /// The path is a `Path<FilePath>`, so the caller has already proven it points at a
    /// file rather than a folder. Unlike the disk backend this cannot fail on invalid
    /// UTF-8, since only valid `String`s are ever stored.
    ///
    /// # Arguments
    /// * `path`: The file to read.
    ///
    /// # Returns
    /// A clone of the stored contents, or a `FileError` if no file is stored at `path`.
    fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError> {
        let path_buf: &PathBuf = path.into();
        self.files.borrow().get(path_buf).cloned().ok_or_else(|| not_found(path_buf))
    }

    /// Reports whether a file is stored at `path`.
    ///
    /// # Arguments
    /// * `path`: The file to check.
    ///
    /// # Returns
    /// `true` if the store holds a file at `path`, otherwise `false`.
    fn exists(&self, path: &Path<FilePath>) -> bool {
        let path_buf: &PathBuf = path.into();
        self.files.borrow().contains_key(path_buf)
    }

    /// Writes contents to `path`, inserting a new entry or replacing an existing one.
    ///
    /// The path is a `Path<FilePath>`, so the caller has already proven it points at a
    /// file. There is no parent folder to be missing, so this always succeeds.
    ///
    /// # Arguments
    /// * `path`: The file to write.
    /// * `data`: The data to be written to the file.
    ///
    /// # Returns
    /// `Ok(())` once the contents are stored.
    fn write_file<X: Into<String>>(&self, path: &Path<FilePath>, data: X) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        self.files.borrow_mut().insert(path_buf.clone(), data.into());
        Ok(())
    }

    /// Deletes the entry stored for `path`.
    ///
    /// The path is a `Path<FilePath>`, so this only ever removes a file, never a folder.
    ///
    /// # Arguments
    /// * `path`: The file to delete.
    ///
    /// # Returns
    /// `Ok(())` once the entry is gone, or a `FileError` if no file is stored at `path`.
    fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError> {
        let path_buf: &PathBuf = path.into();
        self.files.borrow_mut().remove(path_buf).map(|_| ()).ok_or_else(|| not_found(path_buf))
    }

    /// Moves the entry from `from` to `to`, removing the source.
    ///
    /// After a successful move the source no longer exists and its contents are stored at
    /// the destination. Both paths are files, so a folder can never be moved by mistake.
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
        let mut files = self.files.borrow_mut();
        let contents = files.remove(from_buf).ok_or_else(|| not_found(from_buf))?;
        files.insert(to_buf.clone(), contents);
        Ok(())
    }

    /// Copies the entry from `from` to `to`, leaving the source in place.
    ///
    /// After a successful copy both the source and the destination are stored with the
    /// same contents. Both paths are files, so a folder can never be copied by mistake.
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
        let mut files = self.files.borrow_mut();
        let contents = files.get(from_buf).cloned().ok_or_else(|| not_found(from_buf))?;
        files.insert(to_buf.clone(), contents);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The in-memory store needs no filesystem, so each test just builds a fresh
    // `BlockingMemIo` and drives it through the public `FileIo` interface. Effects are
    // checked by reading back through the same interface rather than poking at the map.

    use super::*;

    /// Builds a typed file path from a plain name for use in the tests.
    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
    }

    #[test]
    fn read_file_returns_file_contents() {
        let io = BlockingMemIo::new();
        io.write_file(&file("main.cad"), "hello world").unwrap();

        assert_eq!(io.read_file(&file("main.cad")).unwrap(), "hello world");
    }

    #[test]
    fn read_file_missing_file_errors() {
        let io = BlockingMemIo::new();

        assert!(io.read_file(&file("missing.cad")).is_err());
    }

    #[test]
    fn write_file_creates_new_file() {
        let io = BlockingMemIo::new();
        assert!(io.read_file(&file("new.cad")).is_err());

        io.write_file(&file("new.cad"), "created").unwrap();
        assert_eq!(io.read_file(&file("new.cad")).unwrap(), "created");
    }

    #[test]
    fn write_file_overwrites_existing_file() {
        let io = BlockingMemIo::new();
        io.write_file(&file("existing.cad"), "old contents").unwrap();

        io.write_file(&file("existing.cad"), "new").unwrap();
        // The old contents are fully replaced, not appended to.
        assert_eq!(io.read_file(&file("existing.cad")).unwrap(), "new");
    }

    #[test]
    fn delete_file_removes_file() {
        let io = BlockingMemIo::new();
        io.write_file(&file("doomed.cad"), "bye").unwrap();

        io.delete_file(&file("doomed.cad")).unwrap();
        assert!(io.read_file(&file("doomed.cad")).is_err());
    }

    #[test]
    fn delete_file_missing_file_errors() {
        let io = BlockingMemIo::new();

        assert!(io.delete_file(&file("missing.cad")).is_err());
    }

    #[test]
    fn move_file_moves_contents_and_removes_source() {
        let io = BlockingMemIo::new();
        io.write_file(&file("from.cad"), "payload").unwrap();

        io.move_file(&file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(io.read_file(&file("to.cad")).unwrap(), "payload");
        assert!(io.read_file(&file("from.cad")).is_err());
    }

    #[test]
    fn copy_file_duplicates_contents_and_keeps_source() {
        let io = BlockingMemIo::new();
        io.write_file(&file("from.cad"), "payload").unwrap();

        io.copy_file(&file("from.cad"), &file("to.cad")).unwrap();
        assert_eq!(io.read_file(&file("from.cad")).unwrap(), "payload");
        assert_eq!(io.read_file(&file("to.cad")).unwrap(), "payload");
    }

    #[test]
    fn exists_reports_stored_files() {
        let io = BlockingMemIo::new();
        assert!(!io.exists(&file("main.cad")));
        io.write_file(&file("main.cad"), "x").unwrap();
        assert!(io.exists(&file("main.cad")));
    }
}
