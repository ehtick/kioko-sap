use std::fs;
use std::path::{Path as StdPath, PathBuf};

use crate::files::engines::blocking_io::disk::BlockingDiskIo;
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};
use crate::errors::file::FileError;

/// Wraps a `std::io::Error` into a `FileError::Io`, tagging it with the path it happened on.
///
/// Every folder call funnels its failures through here so the caller gets a consistent
/// `FileError` carrying both the path that failed and the underlying OS message.
///
/// # Arguments
/// * `path`: The folder or file the failing operation was acting on.
/// * `error`: The underlying `std::io::Error` returned by the filesystem call.
///
/// # Returns
/// A `FileError::Io` holding the path and the OS error message.
fn io_error(path: &StdPath, error: std::io::Error) -> FileError {
    FileError::Io { path: path.to_string_lossy().to_string(), message: error.to_string() }
}

/// Wraps a discovered filesystem path in a typed `Path<FilePath>` rooted at `root`.
///
/// The listing preserves the queried folder's `root`, so the returned path's `relative()` is
/// the path relative to that root rather than the absolute disk path, while its full path is
/// unchanged. A file's kind is fixed by the type parameter, so an extensionless name (e.g.
/// `README`) types cleanly.
///
/// # Arguments
/// * `root`: The root the queried folder was built with, carried onto each child.
/// * `entry`: The absolute filesystem path of a file discovered on disk.
///
/// # Returns
/// The path as a root-preserving `Path<FilePath>`.
fn typed_file(root: &StdPath, entry: &StdPath) -> Result<Path<FilePath>, FileError> {
    let rel = entry.strip_prefix(root).unwrap_or(entry);
    Path::<FilePath>::try_from((
        root.to_string_lossy().to_string(),
        rel.to_string_lossy().to_string(),
    ))
}

/// Wraps a discovered filesystem directory in a typed `Path<FolderPath>` rooted at `root`.
///
/// The folder twin of [`typed_file`]; the root is preserved for the same reason. Construction
/// does not depend on the extension, so a dotted directory (`.git`) types cleanly.
///
/// # Arguments
/// * `root`: The root the queried folder was built with, carried onto each child.
/// * `entry`: The absolute filesystem path of a directory discovered on disk.
///
/// # Returns
/// The path as a root-preserving `Path<FolderPath>`.
fn typed_folder(root: &StdPath, entry: &StdPath) -> Result<Path<FolderPath>, FileError> {
    let rel = entry.strip_prefix(root).unwrap_or(entry);
    Path::<FolderPath>::try_from((
        root.to_string_lossy().to_string(),
        rel.to_string_lossy().to_string(),
    ))
}

/// Recursively collects every file under `dir` into `out`, rooting each at `root`.
///
/// Descends into each sub-folder depth-first, pushing only files (never folders). This
/// backs `all_child_files`, which needs the whole subtree rather than the immediate level.
///
/// # Arguments
/// * `root`: The root the queried folder was built with, carried onto each discovered file.
/// * `dir`: The folder to walk.
/// * `out`: The accumulator that every discovered file is pushed onto.
///
/// # Returns
/// `Ok(())` once the subtree is fully walked, or the first `FileError` encountered.
fn collect_files(
    root: &StdPath,
    dir: &StdPath,
    out: &mut Vec<Path<FilePath>>,
) -> Result<(), FileError> {
    for entry in fs::read_dir(dir).map_err(|error| io_error(dir, error))? {
        let entry = entry.map_err(|error| io_error(dir, error))?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_files(root, &entry_path, out)?;
        } else if entry_path.is_file() {
            out.push(typed_file(root, &entry_path)?);
        }
    }
    Ok(())
}

/// Recursively copies the folder tree at `src` into `dst`, creating folders as needed.
///
/// The standard library has no recursive copy, so this walks the source and copies each
/// file into the mirrored location under the destination. This backs `copy_folder`.
///
/// # Arguments
/// * `src`: The existing folder to copy from.
/// * `dst`: The destination folder to copy into.
///
/// # Returns
/// `Ok(())` once the whole tree is copied, or the first `FileError` encountered.
fn copy_dir_all(src: &StdPath, dst: &StdPath) -> Result<(), FileError> {
    fs::create_dir_all(dst).map_err(|error| io_error(dst, error))?;
    for entry in fs::read_dir(src).map_err(|error| io_error(src, error))? {
        let entry = entry.map_err(|error| io_error(src, error))?;
        let entry_path = entry.path();
        let destination = dst.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &destination)?;
        } else {
            fs::copy(&entry_path, &destination).map_err(|error| io_error(&entry_path, error))?;
        }
    }
    Ok(())
}

impl FolderIo for BlockingDiskIo {
    /// Lists the files directly inside `path`, without descending into sub-folders.
    ///
    /// Only immediate children that are files are returned; sub-folders are ignored. The
    /// order follows the operating system's directory order, which is not sorted.
    ///
    /// # Arguments
    /// * `path`: The folder to list.
    ///
    /// # Returns
    /// The immediate child files as `Path<FilePath>`s, or a `FileError` if the folder
    /// cannot be read or a child cannot be represented as a file path.
    fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let root = path.root_path.clone();
        let folder: &PathBuf = (&path).into();
        let mut files = Vec::new();
        for entry in fs::read_dir(folder).map_err(|error| io_error(folder, error))? {
            let entry = entry.map_err(|error| io_error(folder, error))?;
            let entry_path = entry.path();
            if entry_path.is_file() {
                files.push(typed_file(&root, &entry_path)?);
            }
        }
        Ok(files)
    }

    /// Lists every file anywhere beneath `path`, descending into all sub-folders.
    ///
    /// This walks the whole subtree, so a file nested several folders deep is included.
    /// Folders themselves are never returned, only the files they contain.
    ///
    /// # Arguments
    /// * `path`: The folder whose subtree is walked.
    ///
    /// # Returns
    /// Every descendant file as a `Path<FilePath>`, or a `FileError` if any folder cannot
    /// be read or a file cannot be represented as a file path.
    fn all_child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let root = path.root_path.clone();
        let folder: &PathBuf = (&path).into();
        let mut files = Vec::new();
        collect_files(&root, folder, &mut files)?;
        Ok(files)
    }

    /// Lists the immediate sub-folders directly inside `path`, without descending.
    ///
    /// Only immediate children that are directories are returned; files are ignored. The order
    /// follows the operating system's directory order, which is not sorted.
    ///
    /// # Arguments
    /// * `path`: The folder to list.
    ///
    /// # Returns
    /// The immediate child folders as `Path<FolderPath>`s, or a `FileError` if the folder
    /// cannot be read.
    fn child_folders(&self, path: Path<FolderPath>) -> Result<Vec<Path<FolderPath>>, FileError> {
        let root = path.root_path.clone();
        let folder: &PathBuf = (&path).into();
        let mut folders = Vec::new();
        for entry in fs::read_dir(folder).map_err(|error| io_error(folder, error))? {
            let entry = entry.map_err(|error| io_error(folder, error))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                folders.push(typed_folder(&root, &entry_path)?);
            }
        }
        Ok(folders)
    }

    /// Creates the folder at `path` and any missing parents.
    ///
    /// Idempotent: creating a folder that already exists is a success (`fs::create_dir_all`).
    ///
    /// # Arguments
    /// * `path`: The folder to create.
    ///
    /// # Returns
    /// `Ok(())` once the folder exists, or a `FileError` if it could not be created.
    fn create_folder(&self, path: Path<FolderPath>) -> Result<(), FileError> {
        let folder: &PathBuf = (&path).into();
        fs::create_dir_all(folder).map_err(|error| io_error(folder, error))
    }

    /// Reports whether a directory exists on disk at `path`.
    ///
    /// True only for a directory — a regular file at `path` reads as `false`.
    ///
    /// # Arguments
    /// * `path`: The folder to check.
    ///
    /// # Returns
    /// `true` if a directory exists at `path`, otherwise `false`.
    fn folder_exists(&self, path: &Path<FolderPath>) -> bool {
        let folder: &PathBuf = path.into();
        folder.is_dir()
    }

    /// Deletes the folder at `path` and everything inside it.
    ///
    /// The path is a `Path<FolderPath>`, so this only ever removes a folder, never a
    /// single file. The removal is recursive: all nested files and folders go with it.
    ///
    /// # Arguments
    /// * `path`: The folder to delete.
    ///
    /// # Returns
    /// `Ok(())` once the folder is gone, or a `FileError` if it does not exist or could
    /// not be removed.
    fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError> {
        let folder: &PathBuf = (&path).into();
        fs::remove_dir_all(folder).map_err(|error| io_error(folder, error))
    }

    /// Moves the folder from `from` to `to`, taking its whole contents with it.
    ///
    /// After a successful move the source folder no longer exists and its subtree lives
    /// at the destination. Both paths are folders, so a file can never be moved by mistake.
    ///
    /// # Arguments
    /// * `from`: The existing folder to move.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if the source is missing or the destination
    /// could not be written.
    fn move_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        fs::rename(from_buf, to_buf).map_err(|error| io_error(from_buf, error))
    }

    /// Copies the folder from `from` to `to`, leaving the source in place.
    ///
    /// After a successful copy both folders exist and hold the same subtree. Both paths
    /// are folders, so a file can never be copied by mistake.
    ///
    /// # Arguments
    /// * `from`: The existing folder to copy.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if the source is missing or the destination
    /// could not be written.
    fn copy_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        copy_dir_all(from_buf, to_buf)
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

    /// Builds a typed folder path from a filesystem path.
    fn folder(path: &StdPath) -> Path<FolderPath> {
        Path::<FolderPath>::new(path.to_string_lossy().to_string()).unwrap()
    }

    /// Writes `contents` to `dir/relative`, creating any parent folders first, and returns
    /// the full path so the test can assert against it.
    fn seed(dir: &TempDir, relative: &str, contents: &str) -> PathBuf {
        let full = dir.path().join(relative);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, contents).unwrap();
        full
    }

    /// Collects a listing result into a sorted set of raw paths for order-independent asserts.
    fn raw_paths(files: Vec<Path<FilePath>>) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = files
            .iter()
            .map(|file| {
                let buf: &PathBuf = file.into();
                buf.clone()
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn child_files_lists_only_immediate_files() {
        let dir = tempdir().unwrap();
        let a = seed(&dir, "a.cad", "a");
        let b = seed(&dir, "b.cad", "b");
        // A nested file must not appear in the immediate listing.
        seed(&dir, "nested/deep.cad", "deep");

        let mut expected = vec![a, b];
        expected.sort();
        let listed = raw_paths(BlockingDiskIo.child_files(folder(dir.path())).unwrap());
        assert_eq!(listed, expected);
    }

    #[test]
    fn all_child_files_lists_whole_subtree() {
        let dir = tempdir().unwrap();
        let a = seed(&dir, "a.cad", "a");
        let deep = seed(&dir, "nested/deep.cad", "deep");
        let deeper = seed(&dir, "nested/inner/deeper.cad", "deeper");

        let mut expected = vec![a, deep, deeper];
        expected.sort();
        let listed = raw_paths(BlockingDiskIo.all_child_files(folder(dir.path())).unwrap());
        assert_eq!(listed, expected);
    }

    #[test]
    fn delete_folder_removes_folder_and_contents() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("sub");
        seed(&dir, "sub/a.cad", "a");
        assert!(target.exists());

        BlockingDiskIo.delete_folder(folder(&target)).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn delete_folder_missing_folder_errors() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");

        assert!(BlockingDiskIo.delete_folder(folder(&missing)).is_err());
    }

    #[test]
    fn move_folder_moves_subtree_and_removes_source() {
        let dir = tempdir().unwrap();
        seed(&dir, "from/a.cad", "a");
        let from = dir.path().join("from");
        let to = dir.path().join("to");

        BlockingDiskIo.move_folder(folder(&from), folder(&to)).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read_to_string(to.join("a.cad")).unwrap(), "a");
    }

    #[test]
    fn copy_folder_duplicates_subtree_and_keeps_source() {
        let dir = tempdir().unwrap();
        seed(&dir, "from/a.cad", "a");
        seed(&dir, "from/nested/b.cad", "b");
        let from = dir.path().join("from");
        let to = dir.path().join("to");

        BlockingDiskIo.copy_folder(folder(&from), folder(&to)).unwrap();
        // Source is untouched.
        assert_eq!(fs::read_to_string(from.join("a.cad")).unwrap(), "a");
        // Destination mirrors the whole subtree.
        assert_eq!(fs::read_to_string(to.join("a.cad")).unwrap(), "a");
        assert_eq!(fs::read_to_string(to.join("nested/b.cad")).unwrap(), "b");
    }

    #[test]
    fn child_folders_lists_immediate_subfolders_including_dotted() {
        let dir = tempdir().unwrap();
        seed(&dir, "sub_a/x.cad", "x");
        seed(&dir, "sub_b/y.cad", "y");
        // A dotted directory (like `.git`) must be listed now the extension rule is relaxed.
        seed(&dir, ".git/HEAD", "ref");
        // A top-level file must not appear among the folders.
        seed(&dir, "z.cad", "z");

        let mut listed: Vec<PathBuf> = BlockingDiskIo
            .child_folders(folder(dir.path()))
            .unwrap()
            .iter()
            .map(|f| {
                let buf: &PathBuf = f.into();
                buf.clone()
            })
            .collect();
        listed.sort();
        let mut expected =
            vec![dir.path().join(".git"), dir.path().join("sub_a"), dir.path().join("sub_b")];
        expected.sort();
        assert_eq!(listed, expected);
    }

    #[test]
    fn create_folder_makes_the_directory_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("fresh/nested");
        assert!(!target.exists());

        BlockingDiskIo.create_folder(folder(&target)).unwrap();
        assert!(target.is_dir());
        // Creating again is a success, not an error.
        BlockingDiskIo.create_folder(folder(&target)).unwrap();
    }

    #[test]
    fn folder_exists_reports_only_directories() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        assert!(!BlockingDiskIo.folder_exists(&folder(&sub)));
        seed(&dir, "sub/a.cad", "a");
        assert!(BlockingDiskIo.folder_exists(&folder(&sub)));

        // A regular file at the path is not a folder.
        let file_path = seed(&dir, "afile.cad", "x");
        assert!(!BlockingDiskIo.folder_exists(&folder(&file_path)));
    }
}
