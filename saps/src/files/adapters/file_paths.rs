//! Adapters that build the crate's typed paths from a `(root, relative)` pair.
//!
//! Consumers of this crate almost always hold a working-tree root as a `std::path::Path` and a
//! project-relative path as a `&str`, and need a `Path<FilePath>` / `Path<FolderPath>` rooted
//! at that root (so `full_path` hits the backend and `relative()` stays project-relative).
//! These free functions wrap the raw `TryFrom<(String, String)>` constructors so every caller
//! spells that conversion the same way rather than reimplementing it. They return the crate's
//! native [`FileError`]; a caller that surfaces a different error type maps it at its own edge.

use std::path::Path as StdPath;

use crate::errors::file::FileError;

use crate::files::paths::{FilePath, FolderPath, Path};

/// Builds a typed file path rooted at `root`.
///
/// The full path the backend acts on is `root` joined with `rel`; `rel` is what the buffer
/// guard keys on and what the streamer emits, so it stays project-relative. The kind is fixed
/// by the return type, so an extensionless name (`README`, `.gitignore`) is a valid file.
///
/// # Arguments
/// * `root`: The base directory the path is rooted at (for example a working tree).
/// * `rel`: The path within the root that names the file.
///
/// # Returns
/// The typed file path, or a `FileError` if it cannot be constructed.
pub fn file_path(root: &StdPath, rel: &str) -> Result<Path<FilePath>, FileError> {
    Path::<FilePath>::try_from((root.to_string_lossy().to_string(), rel.to_string()))
}

/// Builds a typed folder path rooted at `root`.
///
/// A `rel` of `""` yields the root folder itself, whose full path is `root`.
///
/// # Arguments
/// * `root`: The base directory the path is rooted at (for example a working tree).
/// * `rel`: The path within the root that names the folder.
///
/// # Returns
/// The typed folder path, or a `FileError` if it cannot be constructed.
pub fn folder_path(root: &StdPath, rel: &str) -> Result<Path<FolderPath>, FileError> {
    Path::<FolderPath>::try_from((root.to_string_lossy().to_string(), rel.to_string()))
}

/// Builds a typed file path rooted at `root`, for callers holding the root as a `&str`.
///
/// Same behaviour as [`file_path`], without the caller wrapping its string in a `StdPath`.
///
/// # Arguments
/// * `root`: The base directory the path is rooted at (for example a working tree).
/// * `rel`: The path within the root that names the file.
///
/// # Returns
/// The typed file path, or a `FileError` if it cannot be constructed.
pub fn file_path_str(root: &str, rel: &str) -> Result<Path<FilePath>, FileError> {
    Path::<FilePath>::try_from((root.to_string(), rel.to_string()))
}

/// Builds a typed folder path rooted at `root`, for callers holding the root as a `&str`.
///
/// Same behaviour as [`folder_path`], without the caller wrapping its string in a `StdPath`.
///
/// # Arguments
/// * `root`: The base directory the path is rooted at (for example a working tree).
/// * `rel`: The path within the root that names the folder.
///
/// # Returns
/// The typed folder path, or a `FileError` if it cannot be constructed.
pub fn folder_path_str(root: &str, rel: &str) -> Result<Path<FolderPath>, FileError> {
    Path::<FolderPath>::try_from((root.to_string(), rel.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn file_path_joins_root_and_relative_and_keeps_the_relative() {
        let path = file_path(StdPath::new("/tmp/source"), "src/main.cad").unwrap();
        let full: &PathBuf = (&path).into();
        assert_eq!(full, &PathBuf::from("/tmp/source/src/main.cad"));
        // The relative part stays project-relative (not the full disk path).
        assert_eq!(path.relative_string(), "src/main.cad");
    }

    #[test]
    fn folder_path_empty_relative_is_the_root() {
        let path = folder_path(StdPath::new("/tmp/source"), "").unwrap();
        let full: &PathBuf = (&path).into();
        assert_eq!(full, &PathBuf::from("/tmp/source"));
    }

    #[test]
    fn str_variants_match_the_std_path_variants() {
        let file = file_path_str("/tmp/source", "src/main.cad").unwrap();
        let full: &PathBuf = (&file).into();
        assert_eq!(full, &PathBuf::from("/tmp/source/src/main.cad"));
        assert_eq!(file.relative_string(), "src/main.cad");

        let folder = folder_path_str("/tmp/source", "").unwrap();
        let full: &PathBuf = (&folder).into();
        assert_eq!(full, &PathBuf::from("/tmp/source"));
    }

    #[test]
    fn extensionless_and_dotted_names_are_accepted() {
        assert!(file_path(StdPath::new("/tmp/source"), ".gitignore").is_ok());
        assert!(file_path(StdPath::new("/tmp/source"), "README").is_ok());
        assert!(folder_path(StdPath::new("/tmp/source"), ".git").is_ok());
    }
}
