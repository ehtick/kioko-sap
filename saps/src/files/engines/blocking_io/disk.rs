/// Blocking file IO backed by the real filesystem.
///
/// This is the production `FileIo` implementation: every call goes straight to the
/// operating system and blocks the calling thread until the disk operation finishes.
/// It holds no state, so a single value can be shared and reused for all file access.
///
/// The in-memory sibling (`mem`) implements the same `FileIo` trait for tests, so code
/// written against the trait can swap the real disk for a fake without changing.
#[derive(Debug, Clone)]
pub struct BlockingDiskIo;
