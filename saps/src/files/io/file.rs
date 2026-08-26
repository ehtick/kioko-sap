use crate::errors::file::FileError;

use crate::files::paths::{FilePath, Path};

pub trait FileIo {
    fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError>;

    /// Reports whether a file exists at `path`.
    ///
    /// Returns `bool` rather than `Result` because it is a best-effort pre-check for conflict
    /// detection (destination-taken, source-missing, already-exists); a backend error reads as
    /// "does not exist", and the subsequent real operation still surfaces the underlying
    /// `FileError`.
    fn exists(&self, path: &Path<FilePath>) -> bool;

    fn write_file<X: Into<String>>(&self, path: &Path<FilePath>, data: X) -> Result<(), FileError>;

    fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError>;

    fn move_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError>;

    fn copy_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError>;
}
