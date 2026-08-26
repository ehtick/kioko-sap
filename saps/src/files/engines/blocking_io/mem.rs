use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// In-memory file IO backed by a `HashMap` from path to contents.
///
/// This is a test double for `BlockingDiskIo`: it implements the same `FileIo` trait but
/// keeps every file in memory instead of on the real filesystem, so tests can exercise
/// code that reads and writes files without touching the disk or needing a temp directory.
///
/// The store lives behind a `RefCell` because the `FileIo` methods take `&self` yet the
/// writing operations mutate the map. This makes the type single-threaded (not `Sync`),
/// which is all a blocking in-memory fake needs.
#[derive(Default)]
pub struct BlockingMemIo {
    /// The backing store mapping each file path to its full contents.
    pub files: RefCell<HashMap<PathBuf, String>>,
}

impl BlockingMemIo {
    /// Creates an empty in-memory store with no files.
    ///
    /// # Returns
    /// A `BlockingMemIo` holding no files.
    pub fn new() -> Self {
        Self::default()
    }
}
