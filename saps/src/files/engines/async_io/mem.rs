use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// In-memory async file IO backed by a `HashMap` from path to contents.
///
/// This is the async counterpart of `BlockingMemIo`: a test double for the async backends
/// (such as the browser IndexedDB engine) that keeps every file in memory. Because it needs
/// no browser and no real storage, it lets `AsyncFileIo` consumers be exercised on the host,
/// where the IndexedDB backend cannot run.
///
/// The store lives behind a `Mutex` because the `AsyncFileIo` methods take `&self` yet the
/// writing operations mutate the map. Using a `Mutex` (rather than the blocking engine's
/// `RefCell`) keeps the type `Send`/`Sync`, so it can be driven by any async executor. The
/// guard is only ever held for a single synchronous map operation and never across an await
/// point, so the returned futures stay `Send`.
#[derive(Default)]
pub struct AsyncMemIo {
    /// The backing store mapping each file path to its full contents.
    pub files: Mutex<HashMap<PathBuf, String>>,
}

impl AsyncMemIo {
    /// Creates an empty in-memory store with no files.
    ///
    /// # Returns
    /// An `AsyncMemIo` holding no files.
    pub fn new() -> Self {
        Self::default()
    }
}
