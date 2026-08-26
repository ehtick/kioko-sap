use crate::errors::file::FileError;

use crate::files::paths::{FilePath, FolderPath, Path};

/// The async counterpart to `FolderIo` for backends whose operations are inherently
/// asynchronous (for example browser IndexedDB, which only exposes a callback/event API).
///
/// The method set mirrors `FolderIo` exactly; the only difference is that every operation
/// is an `async fn`. Because the intended backends run single-threaded in the browser, the
/// returned futures are not required to be `Send`.
#[allow(async_fn_in_trait)]
pub trait AsyncFolderIo {
    async fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError>;

    async fn all_child_files(
        &self,
        path: Path<FolderPath>,
    ) -> Result<Vec<Path<FilePath>>, FileError>;

    /// Lists the immediate sub-folders directly inside `path`. Flat key-value backends have no
    /// directory entities and return an empty list (see [`crate::files::io::folder::FolderIo::child_folders`]).
    async fn child_folders(
        &self,
        path: Path<FolderPath>,
    ) -> Result<Vec<Path<FolderPath>>, FileError>;

    /// Creates an empty folder at `path`. A no-op on flat key-value backends (see
    /// [`crate::files::io::folder::FolderIo::create_folder`]).
    async fn create_folder(&self, path: Path<FolderPath>) -> Result<(), FileError>;

    /// Reports whether a folder exists at `path`. Best-effort; a backend error reads as
    /// "does not exist". Named distinctly from `AsyncFileIo::exists` so a store implementing
    /// both traits has no ambiguous `exists` call.
    async fn folder_exists(&self, path: &Path<FolderPath>) -> bool;

    async fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError>;

    async fn move_folder(
        &self,
        from: Path<FolderPath>,
        to: Path<FolderPath>,
    ) -> Result<(), FileError>;

    async fn copy_folder(
        &self,
        from: Path<FolderPath>,
        to: Path<FolderPath>,
    ) -> Result<(), FileError>;
}
