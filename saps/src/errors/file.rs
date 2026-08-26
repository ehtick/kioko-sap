//! The error returned by every file operation.
//!
//! One enum covers the whole `files` module rather than a type per layer,
//! because a caller reading a file does not want to match on a different error
//! depending on whether the bytes came off a disk, a key-value store, or the
//! browser's IndexedDB. The variant says which layer failed and the fields
//! carry the path it failed on plus the backend's own message, verbatim.
//!
//! The backend message is never reformatted on the way through. Callers match
//! on the exact text an operating system or storage engine produced — a missing
//! file really does need to read `No such file or directory (os error 2)` — so
//! wrapping it would break them.
//!
//! [`crate::errors::saps::SapsError`] has a `From` impl for this type, so a
//! server handler can lift a file failure into an HTTP response with `?`.

/// A failure raised by a file, folder, or in-memory buffer operation.
#[derive(thiserror::Error, Debug, Clone)]
pub enum FileError {
    /// The underlying storage engine failed to read or write.
    #[error("File error - {path}:{message}")]
    Io {
        /// The path the operation was aimed at.
        path: String,
        /// The backend's own message, carried through unchanged.
        message: String,
    },
    /// The path itself was not well formed for the operation.
    #[error("File path error - {path}:{message}")]
    Path {
        /// The path that could not be interpreted.
        path: String,
        /// What was wrong with it.
        message: String,
    },
    /// An in-memory buffer operation failed.
    #[error("MemFile error - {path}:{message}")]
    MemFile {
        /// The path of the buffer that failed.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// A file-graph operation failed.
    #[error("File graph error - {message}")]
    Graph {
        /// What went wrong.
        message: String,
    },
}
