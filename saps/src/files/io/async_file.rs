use crate::errors::file::FileError;

use crate::files::paths::{FilePath, Path};

/// The async counterpart to `FileIo` for backends whose operations are inherently
/// asynchronous (for example browser IndexedDB, which only exposes a callback/event API).
///
/// The method set mirrors `FileIo` exactly; the only difference is that every operation is
/// an `async fn`. Because the intended backends run single-threaded in the browser, the
/// returned futures are not required to be `Send`.
#[allow(async_fn_in_trait)]
pub trait AsyncFileIo {
    async fn read_file(&self, path: &Path<FilePath>) -> Result<String, FileError>;

    /// Reports whether a file exists at `path`. Best-effort (see [`crate::files::io::file::FileIo::exists`]):
    /// a backend error reads as "does not exist".
    async fn exists(&self, path: &Path<FilePath>) -> bool;

    async fn write_file<X: Into<String>>(
        &self,
        path: &Path<FilePath>,
        data: X,
    ) -> Result<(), FileError>;

    async fn delete_file(&self, path: &Path<FilePath>) -> Result<(), FileError>;

    async fn move_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError>;

    async fn copy_file(&self, from: &Path<FilePath>, to: &Path<FilePath>) -> Result<(), FileError>;
}
