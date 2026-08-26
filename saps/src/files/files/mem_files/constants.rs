/// The single root text field every file's document stores its contents under.
/// Both replicas of a file must agree on this name to converge, so it is fixed.
pub const CONTENT_FIELD: &str = "content";

/// Number of single character edits that may accumulate before we force a
/// save. Bulk edits ignore this and save straight away.
pub const SAVE_THRESHOLD: usize = 5;
