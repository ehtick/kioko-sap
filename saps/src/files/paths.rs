//! Type-safe file and folder paths.
//!
//! A raw `PathBuf` can point at anything, so nothing stops you handing a folder
//! to code that expects a file. This module encodes the distinction in the type
//! system using the typestate pattern: a `Path<FilePath>` and a `Path<FolderPath>`
//! are different types, so the compiler rejects the mix-up before it can run.
//!
//! Every path is split into three parts. The `root_path` is a fixed base that all
//! paths hang off (for example a project directory). The `rel_path` is the path
//! within that root. The `full_path` is the two joined together and is what actually
//! hits the storage backend. Splitting them lets navigation stay inside the root:
//! walking up from a file can reach the root folder but never climb above it.
//!
//! The kind is carried entirely by the typestate `T`, not inferred from the path text, so an
//! extensionless file (`README`, `.gitignore`) and a dotted folder (`.git`) are both valid.
//! Construction is still fallible because navigation must reject a relative path that escapes
//! its root, and because the `TryFrom` contract needs an error type.
//!
//! # Interface
//! Construction (fallible, validates the extension rule):
//! - `Path::<FilePath>::new` / `Path::<FolderPath>::new` — build from a single string
//!   with an empty root.
//! - `TryFrom<(String, String)>` / `TryFrom<(&String, &String)>` — build from a
//!   `(root_path, rel_path)` pair. Construction is fallible (the extension rule can
//!   reject the input), so this is `TryFrom`, not `From`.
//!
//! Navigation (consumes the path and returns the neighbouring one, preserving the
//! type-level guarantee):
//! - `Path::<FilePath>::into_parent_folder` — drop the file to reach its folder. Errors
//!   if the parent would climb above the root.
//! - `Path::<FolderPath>::into_child_file` — append a file inside the folder.
//! - `Path::<FolderPath>::into_child_folder` — append a nested folder.
//!
//! The wrapped paths are private; a path is only ever obtained through these validated
//! entry points, and the full path is borrowed back out via `From<&Path<_>> for &PathBuf`.
use crate::errors::file::FileError;
use std::{
    hash::{Hash, Hasher},
    marker::PhantomData,
    path::{Component, Path as StdPath, PathBuf},
};

/// A path for a file.
#[derive(Debug, Clone)]
pub struct FilePath;

/// A path for a folder.
#[derive(Debug, Clone)]
pub struct FolderPath;

/// A marker for the type of path.
pub trait PathType {}

// Impl the types of paths.
impl PathType for FilePath {}
impl PathType for FolderPath {}

/// A path for either a file or folder.
#[derive(Debug, Clone)]
pub struct Path<T: PathType> {
    /// The path to the file or folder relative to the root.
    pub rel_path: PathBuf,
    /// The root path that all other paths branch off from.
    pub root_path: PathBuf,
    /// The full path, which is the root path and the relative path joined together.
    pub full_path: PathBuf,
    /// The type of path (typestate pattern).
    pub(crate) path_type: PhantomData<T>,
}

/// Identity is the `full_path` alone: two file paths are equal when they
/// resolve to the same location, regardless of how the root/relative split was
/// built. `root_path`/`rel_path` are construction detail, and `path_type` is a
/// zero-sized typestate marker, so neither participates.
impl PartialEq for Path<FilePath> {
    fn eq(&self, other: &Self) -> bool {
        self.full_path == other.full_path
    }
}

impl Eq for Path<FilePath> {}

/// Hashes on `full_path` only, matching [`PartialEq`] so the `Hash`/`Eq`
/// contract holds and `Path<FilePath>` can key a `HashMap`.
impl Hash for Path<FilePath> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.full_path.hash(state);
    }
}

/// Reports whether a relative path climbs above its root.
///
/// Each component moves a notional depth counter: a normal name goes one level deeper, a
/// `..` goes one level up, and a `.` stays put. If the depth ever drops below zero the
/// path has escaped above the root it is relative to. A root or prefix component (which
/// should never appear in a relative path) is also treated as an escape.
///
/// # Arguments
/// * `rel`: The relative path to inspect.
///
/// # Returns
/// `true` if the path escapes above its root, otherwise `false`.
fn escapes_root(rel: &StdPath) -> bool {
    let mut depth: i32 = 0;
    for component in rel.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            },
            Component::CurDir => {},
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

impl Path<FilePath> {
    /// Builds a file path from a root and a relative path.
    ///
    /// The full path is the root joined with the relative path. The kind (file vs folder) is
    /// carried by the typestate `T`, not inferred from an extension, so an extensionless name
    /// (`README`, `.gitignore`) is a valid file. Construction stays fallible only so the two
    /// `TryFrom` impls can share one signature; it never fails here.
    ///
    /// # Arguments
    /// * `root`: The base path everything hangs off. May be empty.
    /// * `rel`: The path within the root that names the file.
    ///
    /// # Returns
    /// A constructed file path.
    pub fn build(root: String, rel: String) -> Result<Path<FilePath>, FileError> {
        let root_path = PathBuf::from(root);
        let rel_path = PathBuf::from(rel);
        let full_path = root_path.join(&rel_path);
        Ok(Self { rel_path, root_path, full_path, path_type: PhantomData })
    }

    /// The constructor for the `Path<FilePath>` from a single path with an empty root.
    ///
    /// # Arguments
    /// * `path`: The relative path to construct the `Path` from.
    ///
    /// # Returns
    /// A constructed file path.
    pub fn new<X: Into<String>>(path: X) -> Result<Path<FilePath>, FileError> {
        Self::build(String::new(), path.into())
    }

    /// Consumes the file path and returns the folder that contains it.
    ///
    /// This drops the final component (the file itself) and keeps everything before it,
    /// carrying the same root along. For `src/main.cad` the parent folder is `src`; for a
    /// file sitting directly in the root (`main.cad`) the parent is the root folder itself,
    /// which is the empty relative path. If dropping the file name would climb above the
    /// root (for example the relative path starts with `..`) this is an error.
    ///
    /// # Returns
    /// The containing folder as a `Path<FolderPath>`, or a `FileError::Path` if the parent
    /// would escape the root.
    pub fn into_parent_folder(self) -> Result<Path<FolderPath>, FileError> {
        let rel_parent = match self.rel_path.parent() {
            Some(parent) => parent.to_path_buf(),
            None => {
                return Err(FileError::Path {
                    path: self.full_path.to_string_lossy().to_string(),
                    message: "parent folder escapes the root path".into(),
                });
            },
        };
        if escapes_root(&rel_parent) {
            return Err(FileError::Path {
                path: self.full_path.to_string_lossy().to_string(),
                message: "parent folder escapes the root path".into(),
            });
        }
        let full_path = self.root_path.join(&rel_parent);
        Ok(Path {
            rel_path: rel_parent,
            root_path: self.root_path,
            full_path,
            path_type: PhantomData,
        })
    }

    /// The extension of the file.
    ///
    /// # Returns
    /// A string denoting the extension if the extension exists
    pub fn extension(&self) -> Option<String> {
        match self.full_path.extension() {
            Some(value) => Some(value.to_string_lossy().to_string()),
            None => None,
        }
    }

    /// Sets the extension for the path.
    ///
    /// # Arguments
    /// - `ext`: the extension to be set to
    pub fn set_extension(&mut self, ext: &str) {
        self.full_path.set_extension(ext);
    }
}

impl Path<FolderPath> {
    /// Builds a folder path from a root and a relative path.
    ///
    /// The full path is the root joined with the relative path. The kind (file vs folder) is
    /// carried by the typestate `T`, not inferred from an extension, so a dotted name
    /// (`.git`, `my.folder`) is a valid folder. Construction stays fallible only so the two
    /// `TryFrom` impls can share one signature; it never fails here.
    ///
    /// # Arguments
    /// * `root`: The base path everything hangs off. May be empty.
    /// * `rel`: The path within the root that names the folder.
    ///
    /// # Returns
    /// A constructed folder path.
    fn build(root: String, rel: String) -> Result<Path<FolderPath>, FileError> {
        let root_path = PathBuf::from(root);
        let rel_path = PathBuf::from(rel);
        let full_path = root_path.join(&rel_path);
        Ok(Self { rel_path, root_path, full_path, path_type: PhantomData })
    }

    /// The constructor for the `Path<FolderPath>` from a single path with an empty root.
    ///
    /// The kind is fixed by the return type, so `src`, `src/nested`, and even a dotted `.git`
    /// are all valid folders regardless of any extension in the name.
    ///
    /// # Arguments
    /// * `path`: The relative path to construct the `Path` from.
    ///
    /// # Returns
    /// A constructed folder path.
    pub fn new<X: Into<String>>(path: X) -> Result<Path<FolderPath>, FileError> {
        Self::build(String::new(), path.into())
    }

    /// Consumes the folder path and returns a file path nested inside it.
    ///
    /// The file name is joined onto the folder, carrying the same root along, so `src`
    /// with a child `main.cad` gives `src/main.cad`. The result is a file because the
    /// return type says so — an extensionless name (`README`) is a valid child file.
    ///
    /// # Arguments
    /// * `file`: The name of the child file to append to this folder.
    ///
    /// # Returns
    /// The nested file as a `Path<FilePath>`.
    pub fn into_child_file<X: Into<String>>(self, file: X) -> Result<Path<FilePath>, FileError> {
        let file_string = file.into();
        let rel_path = self.rel_path.join(&file_string);
        let full_path = self.full_path.join(&file_string);
        Ok(Path { rel_path, root_path: self.root_path, full_path, path_type: PhantomData })
    }

    /// Consumes the folder path and returns a nested folder inside it.
    ///
    /// The folder name is joined onto this folder, carrying the same root along, so `src`
    /// with a child `nested` gives `src/nested`. The result is a folder because the return
    /// type says so — a dotted name (`.git`, `my.folder`) is a valid child folder.
    ///
    /// # Arguments
    /// * `folder`: The name of the child folder to append to this folder.
    ///
    /// # Returns
    /// The nested folder as a `Path<FolderPath>`.
    pub fn into_child_folder<X: Into<String>>(
        self,
        folder: X,
    ) -> Result<Path<FolderPath>, FileError> {
        let folder_string = folder.into();
        let rel_path = self.rel_path.join(&folder_string);
        let full_path = self.full_path.join(&folder_string);
        Ok(Path { rel_path, root_path: self.root_path, full_path, path_type: PhantomData })
    }
}

impl<T: PathType> Path<T> {
    /// The path relative to its root — the part callers address a file or folder by, without
    /// the root prefix that only matters to the storage backend.
    ///
    /// For a path built with `try_from(("proj", "src/main.cad"))` this is `src/main.cad`, not
    /// the full `proj/src/main.cad`. It is what actor messages and the frontend file tree key
    /// on, and what the mem-file streamer emits per buffer.
    ///
    /// # Returns
    /// The relative path.
    pub fn relative(&self) -> &StdPath {
        &self.rel_path
    }

    /// The relative path as an owned lossy `String`.
    ///
    /// # Returns
    /// The relative path as a string.
    pub fn relative_string(&self) -> String {
        self.rel_path.to_string_lossy().to_string()
    }

    /// The root path as an owned lossy `String`.
    ///
    /// # Returns
    /// The root path as a string
    pub fn root_string(&self) -> String {
        self.root_path.to_string_lossy().to_string()
    }
}

/// Builds a file path from an owned `(root_path, rel_path)` pair.
///
/// Construction is fallible (the `TryFrom` contract needs an error type, and navigation can
/// reject a root escape), so this is `TryFrom` rather than `From`.
impl TryFrom<(String, String)> for Path<FilePath> {
    type Error = FileError;

    fn try_from((root, rel): (String, String)) -> Result<Self, Self::Error> {
        Self::build(root, rel)
    }
}

/// Builds a file path from a borrowed `(&root_path, &rel_path)` pair.
impl TryFrom<(&String, &String)> for Path<FilePath> {
    type Error = FileError;

    fn try_from((root, rel): (&String, &String)) -> Result<Self, Self::Error> {
        Self::build(root.clone(), rel.clone())
    }
}

/// Builds a folder path from an owned `(root_path, rel_path)` pair.
///
/// Construction is fallible (the `TryFrom` contract needs an error type, and navigation can
/// reject a root escape), so this is `TryFrom` rather than `From`.
impl TryFrom<(String, String)> for Path<FolderPath> {
    type Error = FileError;

    fn try_from((root, rel): (String, String)) -> Result<Self, Self::Error> {
        Self::build(root, rel)
    }
}

/// Builds a folder path from a borrowed `(&root_path, &rel_path)` pair.
impl TryFrom<(&String, &String)> for Path<FolderPath> {
    type Error = FileError;

    fn try_from((root, rel): (&String, &String)) -> Result<Self, Self::Error> {
        Self::build(root.clone(), rel.clone())
    }
}

/// Borrows the full `PathBuf` out of a file path.
///
/// The `'a` lifetime ties the borrowed `PathBuf` to the `Path` it came from, so the
/// reference cannot outlive the path that owns it. This is a read-only view of the full
/// path (root and relative joined), which is what a storage backend acts on.
impl<'a> From<&'a Path<FilePath>> for &'a PathBuf {
    fn from(value: &'a Path<FilePath>) -> Self {
        &value.full_path
    }
}

/// Borrows the full `PathBuf` out of a folder path.
impl<'a> From<&'a Path<FolderPath>> for &'a PathBuf {
    fn from(value: &'a Path<FolderPath>) -> Self {
        &value.full_path
    }
}

impl From<&Path<FilePath>> for String {
    fn from(value: &Path<FilePath>) -> Self {
        value.full_path.to_string_lossy().to_string()
    }
}

// MARK: - Tests

#[cfg(test)]
mod tests {

    use std::assert_eq;

    use super::*;

    #[test]
    fn file_path_with_extension_is_ok() {
        let path = Path::<FilePath>::new("src/main.cad").unwrap();
        assert_eq!(path.full_path, PathBuf::from("src/main.cad"));
    }

    #[test]
    fn file_path_without_extension_is_ok() {
        // An extensionless file (e.g. README) is a valid file: the kind is the typestate.
        let path = Path::<FilePath>::new("src/README").unwrap();
        assert_eq!(path.full_path, PathBuf::from("src/README"));
    }

    #[test]
    fn dotfile_is_a_valid_file() {
        // A leading-dot name has no extension in Rust's eyes, but is a real working-tree file.
        for name in [".gitignore", "LICENSE", ".env"] {
            let path = Path::<FilePath>::new(name).unwrap();
            assert_eq!(path.full_path, PathBuf::from(name));
        }
    }

    #[test]
    fn folder_path_without_extension_is_ok() {
        let path = Path::<FolderPath>::new("src/folder").unwrap();
        assert_eq!(path.full_path, PathBuf::from("src/folder"));
    }

    #[test]
    fn folder_path_with_extension_is_ok() {
        // A dotted folder name (`.git`, `my.folder`) is a valid folder now that the kind is
        // determined by the typestate rather than the extension.
        for name in [".git", "src/my.folder"] {
            let path = Path::<FolderPath>::new(name).unwrap();
            assert_eq!(path.full_path, PathBuf::from(name));
        }
    }

    #[test]
    fn try_from_owned_root_and_relative_builds_full_path() {
        let path =
            Path::<FilePath>::try_from(("proj".to_string(), "src/main.cad".to_string())).unwrap();
        assert_eq!(path.root_path, PathBuf::from("proj"));
        assert_eq!(path.rel_path, PathBuf::from("src/main.cad"));
        assert_eq!(path.full_path, PathBuf::from("proj/src/main.cad"));
    }

    #[test]
    fn try_from_borrowed_root_and_relative_builds_full_path() {
        let root = "proj".to_string();
        let rel = "src".to_string();
        let path = Path::<FolderPath>::try_from((&root, &rel)).unwrap();
        assert_eq!(path.root_path, PathBuf::from("proj"));
        assert_eq!(path.rel_path, PathBuf::from("src"));
        assert_eq!(path.full_path, PathBuf::from("proj/src"));
    }

    #[test]
    fn try_from_file_without_extension_is_ok() {
        let path = Path::<FilePath>::try_from(("proj".to_string(), "README".to_string())).unwrap();
        assert_eq!(path.full_path, PathBuf::from("proj/README"));
    }

    #[test]
    fn try_from_folder_with_extension_is_ok() {
        let path =
            Path::<FolderPath>::try_from(("proj".to_string(), "my.folder".to_string())).unwrap();
        assert_eq!(path.full_path, PathBuf::from("proj/my.folder"));
    }

    #[test]
    fn relative_returns_the_rel_component_not_the_full_path() {
        let file =
            Path::<FilePath>::try_from(("proj".to_string(), "src/main.cad".to_string())).unwrap();
        assert_eq!(file.relative(), StdPath::new("src/main.cad"));
        assert_eq!(file.relative_string(), "src/main.cad");

        let folder = Path::<FolderPath>::try_from(("proj".to_string(), "src".to_string())).unwrap();
        assert_eq!(folder.relative_string(), "src");
    }

    #[test]
    fn into_parent_folder_drops_the_file() {
        let path = Path::<FilePath>::new("src/main.cad").unwrap();
        let folder_path = path.into_parent_folder().unwrap();
        assert_eq!(folder_path.full_path, PathBuf::from("src"));
    }

    #[test]
    fn into_parent_folder_nested() {
        let path = Path::<FilePath>::new("a/b/c/main.cad").unwrap();
        let folder_path = path.into_parent_folder().unwrap();
        assert_eq!(folder_path.full_path, PathBuf::from("a/b/c"));
    }

    #[test]
    fn into_parent_folder_root_level_file_is_the_root() {
        let path = Path::<FilePath>::new("main.cad").unwrap();
        let folder_path = path.into_parent_folder().unwrap();
        // The parent of a file sitting directly in the (empty) root is the root itself.
        assert_eq!(folder_path.full_path, PathBuf::from(""));
    }

    #[test]
    fn into_parent_folder_keeps_the_root() {
        let path =
            Path::<FilePath>::try_from(("proj".to_string(), "src/main.cad".to_string())).unwrap();
        let folder_path = path.into_parent_folder().unwrap();
        assert_eq!(folder_path.root_path, PathBuf::from("proj"));
        assert_eq!(folder_path.rel_path, PathBuf::from("src"));
        assert_eq!(folder_path.full_path, PathBuf::from("proj/src"));
    }

    #[test]
    fn into_parent_folder_escaping_root_errors() {
        // A relative path that climbs out of the root with `..` must not produce a parent
        // above the root.
        let path =
            Path::<FilePath>::try_from(("proj".to_string(), "../main.cad".to_string())).unwrap();
        match path.into_parent_folder() {
            Err(FileError::Path { message, .. }) => {
                assert_eq!(message, "parent folder escapes the root path");
            },
            Err(other) => panic!("expected FileError::Path, got {other:?}"),
            Ok(_) => panic!("expected an error when the parent escapes the root"),
        }
    }

    #[test]
    fn into_child_file_with_extension_is_ok() {
        let folder = Path::<FolderPath>::new("src").unwrap();
        let file = folder.into_child_file("main.cad").unwrap();
        assert_eq!(file.full_path, PathBuf::from("src/main.cad"));
    }

    #[test]
    fn into_child_file_without_extension_is_ok() {
        let folder = Path::<FolderPath>::new("src").unwrap();
        let file = folder.into_child_file("README").unwrap();
        assert_eq!(file.full_path, PathBuf::from("src/README"));
    }

    #[test]
    fn into_child_folder_without_extension_is_ok() {
        let folder = Path::<FolderPath>::new("src").unwrap();
        let child = folder.into_child_folder("nested").unwrap();
        assert_eq!(child.full_path, PathBuf::from("src/nested"));
    }

    #[test]
    fn into_child_folder_with_extension_is_ok() {
        let folder = Path::<FolderPath>::new("src").unwrap();
        let child = folder.into_child_folder("my.folder").unwrap();
        assert_eq!(child.full_path, PathBuf::from("src/my.folder"));
    }

    #[test]
    fn child_navigation_keeps_the_root() {
        let folder = Path::<FolderPath>::try_from(("proj".to_string(), "src".to_string())).unwrap();
        let file = folder.into_child_file("main.cad").unwrap();
        assert_eq!(file.root_path, PathBuf::from("proj"));
        assert_eq!(file.rel_path, PathBuf::from("src/main.cad"));
        assert_eq!(file.full_path, PathBuf::from("proj/src/main.cad"));
    }

    #[test]
    fn test_extension() {
        let folder = Path::<FolderPath>::try_from(("proj".to_string(), "src".to_string())).unwrap();
        let file = folder.into_child_file("main.cad").unwrap();

        assert_eq!("cad", file.extension().expect("extension to be present"));

        let folder = Path::<FolderPath>::try_from(("proj".to_string(), "src".to_string())).unwrap();
        let file = folder.into_child_file("main").unwrap();
        assert_eq!(None, file.extension());
    }
}
