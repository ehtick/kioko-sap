use crate::errors::file::FileError;

use crate::files::paths::{FilePath, FolderPath, Path};

pub trait FolderIo {
    fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError>;

    fn all_child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError>;

    /// Lists the immediate sub-folders directly inside `path`, without descending.
    ///
    /// Flat key-value backends (in-memory, redb, IndexedDB) have no directory entities, so they
    /// return an empty list; only a real filesystem backend enumerates folders. Callers that
    /// need the directory structure must use a disk-backed store.
    fn child_folders(&self, path: Path<FolderPath>) -> Result<Vec<Path<FolderPath>>, FileError>;

    /// Creates an empty folder at `path` (and any missing parents).
    ///
    /// On a real filesystem this materialises the directory. Flat key-value backends have no
    /// directory entities — a folder only exists implicitly once a file is written beneath it —
    /// so they treat this as a successful no-op.
    fn create_folder(&self, path: Path<FolderPath>) -> Result<(), FileError>;

    /// Reports whether a folder exists at `path`. Best-effort, like [`FileIo::exists`]: a
    /// backend error reads as "does not exist". Named distinctly from `FileIo::exists` so a
    /// store implementing both traits has no ambiguous `exists` call.
    fn folder_exists(&self, path: &Path<FolderPath>) -> bool;

    fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError>;

    fn move_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError>;

    fn copy_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError>;
}
