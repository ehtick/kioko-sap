use std::path::Path as StdPath;

use crate::errors::file::FileError;
use redb::{Database, TableDefinition};

/// The single redb table that stores every file, keyed by its path string.
///
/// Both the file IO and folder IO impls open this same table, so it is shared here rather
/// than redefined in each. The key is the file's path as a string; the value is the file's
/// full contents.
pub(crate) const FILES_TABLE: TableDefinition<&str, &str> = TableDefinition::new("files");

/// Wraps any redb error into a `FileError::Io`, tagging it with the path it happened on.
///
/// redb surfaces a family of distinct error types (database, transaction, table, storage,
/// commit). They all implement `Display`, so every KV call funnels its failures through
/// here to get a single consistent `FileError` shape carrying the path and the message.
///
/// # Arguments
/// * `context`: The path (file or folder) the failing operation was acting on.
/// * `error`: Any redb error, taken by its `Display` text.
///
/// # Returns
/// A `FileError::Io` holding the context path and the redb error message.
/// Public so that stores layered on top of this engine in other crates can reuse it
/// rather than re-deriving the same mapping.
pub fn kv_error<E: std::fmt::Display>(context: &str, error: E) -> FileError {
    FileError::Io { path: context.to_string(), message: error.to_string() }
}

/// Blocking file IO backed by a redb key-value database.
///
/// This is a persistent middle ground between the two other engines: like `BlockingMemIo`
/// it keys files by their path, but the store is an on-disk redb database instead of an
/// in-memory map, so the files survive across process runs. redb manages its own
/// concurrency internally, so unlike the in-memory engine this needs no `RefCell` and a
/// single value can be shared for all access.
///
/// The file and folder behaviour is implemented in the `files` and `folders` sibling
/// modules; this type only owns the database handle and the shared table definition.
pub struct KvBlockingIo {
    /// The open redb database that holds the files table.
    pub(crate) db: Database,
}

impl KvBlockingIo {
    /// Opens (or creates) the redb database at `path` and ensures the files table exists.
    ///
    /// The files table is opened once here and committed so that later read transactions
    /// never fail with "table does not exist" on a brand-new database.
    ///
    /// # Arguments
    /// * `path`: Where the redb database file lives. It is created if it does not exist.
    ///
    /// # Returns
    /// A ready `KvBlockingIo`, or a `FileError` if the database could not be opened or the
    /// table could not be created.
    pub fn new<P: AsRef<StdPath>>(path: P) -> Result<Self, FileError> {
        let path = path.as_ref();
        let context = path.to_string_lossy().to_string();

        let db = Database::create(path).map_err(|error| kv_error(&context, error))?;
        // Create the table up front so read transactions on a fresh database succeed.
        let write_txn = db.begin_write().map_err(|error| kv_error(&context, error))?;
        {
            write_txn.open_table(FILES_TABLE).map_err(|error| kv_error(&context, error))?;
        }
        write_txn.commit().map_err(|error| kv_error(&context, error))?;

        Ok(Self { db })
    }
}
