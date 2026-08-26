use std::path::PathBuf;

use redb::{ReadableDatabase, ReadableTable};

use crate::files::engines::blocking_io::kv::{FILES_TABLE, KvBlockingIo, kv_error};
use crate::files::io::folder::FolderIo;
use crate::files::paths::{FilePath, FolderPath, Path};
use crate::errors::file::FileError;

/// Builds a `FileError::Io` for an operation that targeted a folder the database does not hold.
///
/// The database stores only files, so a folder "exists" precisely when at least one file
/// is stored beneath it. When no key has the requested prefix there is nothing to act on,
/// so this raises the same shape of error the other backends use for a missing folder.
///
/// # Arguments
/// * `folder`: The folder that was expected to hold files but was empty or absent.
///
/// # Returns
/// A `FileError::Io` naming the missing folder.
fn not_found(folder: &str) -> FileError {
    FileError::Io { path: folder.to_string(), message: "folder not found".into() }
}

/// Wraps a stored key in a typed `Path<FilePath>`.
///
/// Every key was inserted through the file writer, which only accepts paths that already
/// carry an extension, so this conversion never fails in practice. It still returns a
/// `Result` so a future key that breaks that invariant surfaces rather than panics.
///
/// # Arguments
/// * `key`: A path key read out of the database.
///
/// # Returns
/// The key as a `Path<FilePath>`, or a `FileError` if it has no extension.
fn typed_file(key: &PathBuf) -> Result<Path<FilePath>, FileError> {
    Path::<FilePath>::new(key.to_string_lossy().to_string())
}

impl FolderIo for KvBlockingIo {
    /// Lists the files stored directly inside `path`, without descending into sub-folders.
    ///
    /// The database is a flat table with no explicit folders, so a file is an immediate
    /// child of `path` when its parent equals `path`. A file nested deeper is left out.
    ///
    /// # Arguments
    /// * `path`: The folder to list.
    ///
    /// # Returns
    /// The immediate child files as `Path<FilePath>`s. The order follows the database's
    /// key order, which is sorted lexicographically.
    fn child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let context = folder.to_string_lossy().to_string();

        let read_txn = self.db.begin_read().map_err(|error| kv_error(&context, error))?;
        let table = read_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
        let mut result = Vec::new();
        for row in table.iter().map_err(|error| kv_error(&context, error))? {
            let (key_guard, _value) = row.map_err(|error| kv_error(&context, error))?;
            let key = PathBuf::from(key_guard.value());
            if key.parent() == Some(folder.as_path()) {
                result.push(typed_file(&key)?);
            }
        }
        Ok(result)
    }

    /// Lists every file stored anywhere beneath `path`.
    ///
    /// Because folders are implied by key prefixes, this returns every key that has `path`
    /// as a leading path prefix, however deeply nested.
    ///
    /// # Arguments
    /// * `path`: The folder whose subtree is listed.
    ///
    /// # Returns
    /// Every descendant file as a `Path<FilePath>`, in the database's sorted key order.
    fn all_child_files(&self, path: Path<FolderPath>) -> Result<Vec<Path<FilePath>>, FileError> {
        let folder: &PathBuf = (&path).into();
        let context = folder.to_string_lossy().to_string();

        let read_txn = self.db.begin_read().map_err(|error| kv_error(&context, error))?;
        let table = read_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
        let mut result = Vec::new();
        for row in table.iter().map_err(|error| kv_error(&context, error))? {
            let (key_guard, _value) = row.map_err(|error| kv_error(&context, error))?;
            let key = PathBuf::from(key_guard.value());
            if key.starts_with(folder) {
                result.push(typed_file(&key)?);
            }
        }
        Ok(result)
    }

    /// Lists the immediate sub-folders inside `path`.
    ///
    /// The database is a flat table with no directory entities, so there are no folders to
    /// enumerate — this always returns an empty list. A caller that needs the directory
    /// structure must use a disk-backed store.
    ///
    /// # Arguments
    /// * `path`: The folder to list (ignored).
    ///
    /// # Returns
    /// An empty vector.
    fn child_folders(&self, _path: Path<FolderPath>) -> Result<Vec<Path<FolderPath>>, FileError> {
        Ok(Vec::new())
    }

    /// Creates a folder at `path`.
    ///
    /// A no-op: the flat table has no directory entities, so a folder exists only implicitly
    /// once a file is written beneath it. Nothing is materialised and no spurious key is added.
    ///
    /// # Arguments
    /// * `path`: The folder to create (ignored).
    ///
    /// # Returns
    /// `Ok(())`.
    fn create_folder(&self, _path: Path<FolderPath>) -> Result<(), FileError> {
        Ok(())
    }

    /// Reports whether any file is stored beneath `path` (what "folder exists" means for the
    /// flat table). Best-effort: a database error reads as "does not exist".
    ///
    /// # Arguments
    /// * `path`: The folder to check.
    ///
    /// # Returns
    /// `true` if at least one key has `path` as a leading prefix, otherwise `false`.
    fn folder_exists(&self, path: &Path<FolderPath>) -> bool {
        let folder: &PathBuf = path.into();
        let context = folder.to_string_lossy().to_string();
        let lookup = || -> Result<bool, FileError> {
            let read_txn = self.db.begin_read().map_err(|error| kv_error(&context, error))?;
            let table =
                read_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
            for row in table.iter().map_err(|error| kv_error(&context, error))? {
                let (key_guard, _value) = row.map_err(|error| kv_error(&context, error))?;
                if PathBuf::from(key_guard.value()).starts_with(folder) {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        lookup().unwrap_or(false)
    }

    /// Deletes every file stored beneath `path`.
    ///
    /// Since the database has no standalone folders, deleting a folder means removing every
    /// key under its prefix. All the removals happen in one transaction, so the deletion is
    /// atomic. If no file is stored beneath `path` there is nothing to delete, which is
    /// treated as a missing folder.
    ///
    /// # Arguments
    /// * `path`: The folder to delete.
    ///
    /// # Returns
    /// `Ok(())` once every file beneath `path` is gone, or a `FileError` if the folder
    /// holds no files.
    fn delete_folder(&self, path: Path<FolderPath>) -> Result<(), FileError> {
        let folder: &PathBuf = (&path).into();
        let context = folder.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&context, error))?;
        let removed = {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
            let keys = matching_keys(&table, folder, &context)?;
            for key in &keys {
                table.remove(key.as_str()).map_err(|error| kv_error(&context, error))?;
            }
            keys.len()
        };
        if removed == 0 {
            // Drop the transaction without committing so nothing changes.
            return Err(not_found(&context));
        }
        write_txn.commit().map_err(|error| kv_error(&context, error))?;
        Ok(())
    }

    /// Moves every file beneath `from` so it sits beneath `to` instead.
    ///
    /// Each matching key has its `from` prefix swapped for `to`, and the original key is
    /// removed, all in one transaction. If no file is stored beneath `from` there is
    /// nothing to move, which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to move.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    fn move_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let context = from_buf.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&context, error))?;
        let moved = {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
            let keys = matching_keys(&table, from_buf, &context)?;
            for key in &keys {
                re_key(&mut table, key, from_buf, to_buf, &context)?;
            }
            keys.len()
        };
        if moved == 0 {
            return Err(not_found(&context));
        }
        write_txn.commit().map_err(|error| kv_error(&context, error))?;
        Ok(())
    }

    /// Copies every file beneath `from` so it also sits beneath `to`, keeping the source.
    ///
    /// Each matching key is duplicated with its `from` prefix swapped for `to`; the source
    /// keys stay in place. If no file is stored beneath `from` there is nothing to copy,
    /// which is treated as a missing folder.
    ///
    /// # Arguments
    /// * `from`: The existing folder to copy.
    /// * `to`: The destination folder path.
    ///
    /// # Returns
    /// `Ok(())` on success, or a `FileError` if `from` holds no files.
    fn copy_folder(&self, from: Path<FolderPath>, to: Path<FolderPath>) -> Result<(), FileError> {
        let from_buf: &PathBuf = (&from).into();
        let to_buf: &PathBuf = (&to).into();
        let context = from_buf.to_string_lossy().to_string();

        let write_txn = self.db.begin_write().map_err(|error| kv_error(&context, error))?;
        let copied = {
            let mut table =
                write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
            let keys = matching_keys(&table, from_buf, &context)?;
            for key in &keys {
                // Read the source contents then insert them under the new key, leaving the
                // original in place.
                let contents =
                    match table.get(key.as_str()).map_err(|error| kv_error(&context, error))? {
                        Some(guard) => guard.value().to_string(),
                        None => continue,
                    };
                let new_key = swap_prefix(key, from_buf, to_buf);
                table
                    .insert(new_key.to_string_lossy().as_ref(), contents.as_str())
                    .map_err(|error| kv_error(&context, error))?;
            }
            keys.len()
        };
        if copied == 0 {
            return Err(not_found(&context));
        }
        write_txn.commit().map_err(|error| kv_error(&context, error))?;
        Ok(())
    }
}

/// Collects every key in the table that sits beneath `folder`, as owned strings.
///
/// The keys are copied out so the caller can mutate the table afterwards without holding
/// an iterator borrow over it.
///
/// # Arguments
/// * `table`: The open files table to scan.
/// * `folder`: The folder prefix each key must start with.
/// * `context`: The path used to tag any error raised while scanning.
///
/// # Returns
/// The matching keys as owned strings, or a `FileError` if the scan fails.
fn matching_keys(
    table: &redb::Table<'_, &str, &str>,
    folder: &PathBuf,
    context: &str,
) -> Result<Vec<String>, FileError> {
    let mut keys = Vec::new();
    for row in table.iter().map_err(|error| kv_error(context, error))? {
        let (key_guard, _value) = row.map_err(|error| kv_error(context, error))?;
        let key = PathBuf::from(key_guard.value());
        if key.starts_with(folder) {
            keys.push(key_guard.value().to_string());
        }
    }
    Ok(keys)
}

/// Rewrites `key` so its `from` prefix becomes `to`.
///
/// # Arguments
/// * `key`: The existing key string, known to start with `from`.
/// * `from`: The prefix currently on the key.
/// * `to`: The prefix to put in its place.
///
/// # Returns
/// The key with its prefix swapped.
fn swap_prefix(key: &str, from: &PathBuf, to: &PathBuf) -> PathBuf {
    let key_path = PathBuf::from(key);
    // The caller only passes keys that start with `from`, so the strip cannot fail.
    let relative = key_path.strip_prefix(from).unwrap();
    to.join(relative)
}

/// Moves a single key beneath `from` to sit beneath `to`, removing the original.
///
/// # Arguments
/// * `table`: The open files table to mutate.
/// * `key`: The source key string, known to start with `from`.
/// * `from`: The prefix currently on the key.
/// * `to`: The prefix the moved key should carry.
/// * `context`: The path used to tag any error raised while mutating.
///
/// # Returns
/// `Ok(())` once the key is moved, or a `FileError` if the read/remove/insert fails.
fn re_key(
    table: &mut redb::Table<'_, &str, &str>,
    key: &str,
    from: &PathBuf,
    to: &PathBuf,
    context: &str,
) -> Result<(), FileError> {
    let contents = match table.get(key).map_err(|error| kv_error(context, error))? {
        Some(guard) => guard.value().to_string(),
        None => return Ok(()),
    };
    table.remove(key).map_err(|error| kv_error(context, error))?;
    let new_key = swap_prefix(key, from, to);
    table
        .insert(new_key.to_string_lossy().as_ref(), contents.as_str())
        .map_err(|error| kv_error(context, error))?;
    Ok(())
}

// Filesystem-backed: these drive a real temp directory, so they are native-only.
// A `--target wasm32-unknown-unknown` test build compiles every test module,
// and `tempfile` has no meaning there.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    // Each test opens a fresh redb database inside a `tempfile::tempdir()`, seeds it through
    // the file writer, and drives the folder operations against it.

    use super::*;
    use crate::files::io::file::FileIo;
    use tempfile::{TempDir, tempdir};

    /// Builds a KV engine over a new database inside `dir`.
    fn kv(dir: &TempDir) -> KvBlockingIo {
        KvBlockingIo::new(dir.path().join("test.redb")).unwrap()
    }

    /// Builds a typed folder path from a plain name.
    fn folder(name: &str) -> Path<FolderPath> {
        Path::<FolderPath>::new(name).unwrap()
    }

    /// Builds a typed file path from a plain name.
    fn file(name: &str) -> Path<FilePath> {
        Path::<FilePath>::new(name).unwrap()
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
        let io = kv(&dir);
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/b.cad"), "b").unwrap();
        // A nested file must not appear in the immediate listing.
        io.write_file(&file("src/nested/deep.cad"), "deep").unwrap();

        let listed = raw_paths(io.child_files(folder("src")).unwrap());
        assert_eq!(listed, vec![PathBuf::from("src/a.cad"), PathBuf::from("src/b.cad")]);
    }

    #[test]
    fn all_child_files_lists_whole_subtree() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/nested/deep.cad"), "deep").unwrap();
        io.write_file(&file("src/nested/inner/deeper.cad"), "deeper").unwrap();

        let listed = raw_paths(io.all_child_files(folder("src")).unwrap());
        assert_eq!(
            listed,
            vec![
                PathBuf::from("src/a.cad"),
                PathBuf::from("src/nested/deep.cad"),
                PathBuf::from("src/nested/inner/deeper.cad"),
            ]
        );
    }

    #[test]
    fn delete_folder_removes_all_files_beneath() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("src/a.cad"), "a").unwrap();
        io.write_file(&file("src/nested/b.cad"), "b").unwrap();
        // A sibling folder must be left untouched.
        io.write_file(&file("other/c.cad"), "c").unwrap();

        io.delete_folder(folder("src")).unwrap();
        assert!(io.read_file(&file("src/a.cad")).is_err());
        assert!(io.read_file(&file("src/nested/b.cad")).is_err());
        assert_eq!(io.read_file(&file("other/c.cad")).unwrap(), "c");
    }

    #[test]
    fn delete_folder_missing_folder_errors() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);

        assert!(io.delete_folder(folder("missing")).is_err());
    }

    #[test]
    fn move_folder_moves_subtree_and_removes_source() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("from/a.cad"), "a").unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").unwrap();

        io.move_folder(folder("from"), folder("to")).unwrap();
        assert_eq!(io.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).unwrap(), "b");
        assert!(io.read_file(&file("from/a.cad")).is_err());
        assert!(io.read_file(&file("from/nested/b.cad")).is_err());
    }

    #[test]
    fn move_folder_missing_folder_errors() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);

        assert!(io.move_folder(folder("missing"), folder("to")).is_err());
    }

    #[test]
    fn copy_folder_duplicates_subtree_and_keeps_source() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.write_file(&file("from/a.cad"), "a").unwrap();
        io.write_file(&file("from/nested/b.cad"), "b").unwrap();

        io.copy_folder(folder("from"), folder("to")).unwrap();
        // Source is untouched.
        assert_eq!(io.read_file(&file("from/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("from/nested/b.cad")).unwrap(), "b");
        // Destination mirrors the whole subtree.
        assert_eq!(io.read_file(&file("to/a.cad")).unwrap(), "a");
        assert_eq!(io.read_file(&file("to/nested/b.cad")).unwrap(), "b");
    }

    #[test]
    fn folder_exists_is_true_when_a_file_is_stored_beneath() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        assert!(!io.folder_exists(&folder("src")));
        io.write_file(&file("src/a.cad"), "a").unwrap();
        assert!(io.folder_exists(&folder("src")));
    }

    #[test]
    fn create_folder_and_child_folders_are_flat_no_ops() {
        let dir = tempdir().unwrap();
        let io = kv(&dir);
        io.create_folder(folder("empty")).unwrap();
        assert!(io.all_child_files(folder("empty")).unwrap().is_empty());
        io.write_file(&file("src/nested/a.cad"), "a").unwrap();
        assert!(io.child_folders(folder("src")).unwrap().is_empty());
    }
}
