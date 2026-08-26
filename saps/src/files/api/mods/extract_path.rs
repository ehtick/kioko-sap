use crate::errors::file::FileError;

use crate::files::adapters::file_paths::file_path_str;
use crate::files::api::files::exists;
use crate::files::files::mem_files::async_guard::AsyncMemFileGuard;
use crate::files::files::mem_files::guard::MemFileGuard;
use crate::files::io::async_file::AsyncFileIo;
use crate::files::io::file::FileIo;
use crate::files::paths::{FilePath, Path};

/// Resolves a `mod` declaration to the file it declares through a blocking IO handle.
///
/// # Arguments
/// * `handle`: The blocking IO backend to probe through.
/// * `root_path`: The root of the project the declaration resolves inside.
/// * `rel_path`: The declared module relative to the root, in file form or as a stem.
///
/// # Returns
/// The declared file as whichever candidate exists, or a `FileError` if neither does.
pub fn blocking<H: FileIo>(
    handle: &H,
    root_path: &str,
    rel_path: &str,
) -> Result<Path<FilePath>, FileError> {
    let (direct_path, mod_path) = candidates(root_path, rel_path)?;

    if exists::blocking(handle, &direct_path) {
        return Ok(direct_path);
    }
    if exists::blocking(handle, &mod_path) {
        return Ok(mod_path);
    }
    Err(not_found(rel_path, &direct_path, &mod_path))
}

/// Resolves a `mod` declaration to the file it declares through an async IO handle.
///
/// # Arguments
/// * `handle`: The async IO backend to probe through.
/// * `root_path`: The root of the project the declaration resolves inside.
/// * `rel_path`: The declared module relative to the root, in file form or as a stem.
///
/// # Returns
/// The declared file as whichever candidate exists, or a `FileError` if neither does.
pub async fn asynchronous<H: AsyncFileIo>(
    handle: &H,
    root_path: &str,
    rel_path: &str,
) -> Result<Path<FilePath>, FileError> {
    let (direct_path, mod_path) = candidates(root_path, rel_path)?;

    if exists::asynchronous(handle, &direct_path).await {
        return Ok(direct_path);
    }
    if exists::asynchronous(handle, &mod_path).await {
        return Ok(mod_path);
    }
    Err(not_found(rel_path, &direct_path, &mod_path))
}

/// Resolves a `mod` declaration to the file it declares through a blocking mem-file guard.
///
/// The candidates are probed against the session's view of the project: a live buffer counts
/// as an existing file even before it has flushed to the store, so a module created this
/// session resolves the same way as one already on disk.
///
/// # Arguments
/// * `guard`: The blocking guard owning the live buffers.
/// * `root_path`: The root of the project the declaration resolves inside.
/// * `rel_path`: The declared module relative to the root, in file form or as a stem.
///
/// # Returns
/// The declared file as whichever candidate exists, or a `FileError` if neither does.
pub fn memfile_blocking<S: FileIo>(
    guard: &MemFileGuard<'_, S>,
    root_path: &str,
    rel_path: &str,
) -> Result<Path<FilePath>, FileError> {
    let (direct_path, mod_path) = candidates(root_path, rel_path)?;

    if exists::memfile_blocking(guard, &direct_path) {
        return Ok(direct_path);
    }
    if exists::memfile_blocking(guard, &mod_path) {
        return Ok(mod_path);
    }
    Err(not_found(rel_path, &direct_path, &mod_path))
}

/// Resolves a `mod` declaration to the file it declares through an async mem-file guard.
///
/// The async counterpart of `memfile_blocking`, generic over `AsyncFileIo` so it serves the
/// browser IndexedDB backend and any other async store through the one call.
///
/// # Arguments
/// * `guard`: The async guard owning the live buffers.
/// * `root_path`: The root of the project the declaration resolves inside.
/// * `rel_path`: The declared module relative to the root, in file form or as a stem.
///
/// # Returns
/// The declared file as whichever candidate exists, or a `FileError` if neither does.
pub async fn memfile_asynchronous<S: AsyncFileIo>(
    guard: &AsyncMemFileGuard<'_, S>,
    root_path: &str,
    rel_path: &str,
) -> Result<Path<FilePath>, FileError> {
    let (direct_path, mod_path) = candidates(root_path, rel_path)?;

    if exists::memfile_asynchronous(guard, &direct_path).await {
        return Ok(direct_path);
    }
    if exists::memfile_asynchronous(guard, &mod_path).await {
        return Ok(mod_path);
    }
    Err(not_found(rel_path, &direct_path, &mod_path))
}

/// Builds the two candidate paths a `mod` declaration can resolve to.
///
/// # Arguments
/// * `root_path`: The root of the project the declaration resolves inside.
/// * `rel_path`: The declared module relative to the root, in file form or as a stem.
///
/// # Returns
/// The file form candidate and the directory form candidate, in probe order.
fn candidates(
    root_path: &str,
    rel_path: &str,
) -> Result<(Path<FilePath>, Path<FilePath>), FileError> {
    // A `.cad` suffix is the file form of the same declaration, so it strips back to the
    // stem both candidates build on
    let stem = rel_path.strip_suffix(".cad").unwrap_or(rel_path);
    let direct_path = file_path_str(root_path, &format!("{stem}.cad"))?;
    let mod_path = file_path_str(root_path, &format!("{stem}/mod.cad"))?;
    Ok((direct_path, mod_path))
}

/// Builds the error raised when neither candidate exists.
///
/// # Arguments
/// * `rel_path`: The declared module as the caller passed it in.
/// * `direct_path`: The file form candidate that was probed first.
/// * `mod_path`: The directory form candidate that was probed second.
///
/// # Returns
/// The failure as a `FileError` naming both probed candidates.
fn not_found(rel_path: &str, direct_path: &Path<FilePath>, mod_path: &Path<FilePath>) -> FileError {
    FileError::Path {
        path: rel_path.to_string(),
        message: format!(
            "declared mod file not found: neither {:?} nor {:?} exists",
            direct_path.full_path, mod_path.full_path
        ),
    }
}

#[cfg(test)]
mod tests {
    // See `read.rs` for why only the blocking path is covered here.

    use super::*;
    use crate::files::engines::blocking_io::mem::BlockingMemIo;

    /// Writes an empty project file into the in-memory store under the `project` root.
    ///
    /// # Arguments
    /// * `handle`: The in-memory store to write into.
    /// * `rel_path`: The project relative path to write the file at.
    fn write(handle: &BlockingMemIo, rel_path: &str) {
        let path = file_path_str("project", rel_path).expect("construct file path");
        handle.write_file(&path, "x").expect("write project file");
    }

    #[test]
    fn blocking_picks_file_form_when_present() {
        let handle = BlockingMemIo::new();
        write(&handle, "points/one.cad");

        let resolved = blocking(&handle, "project", "points/one.cad").expect("resolve mod");

        // The root and relative halves survive the resolution so the relative path can key
        // walker maps and buffer guards
        assert_eq!(resolved.relative_string(), "points/one.cad");
        assert_eq!(resolved.root_string(), "project");
    }

    #[test]
    fn blocking_falls_back_to_mod_cad() {
        let handle = BlockingMemIo::new();
        write(&handle, "points/one/mod.cad");

        let resolved = blocking(&handle, "project", "points/one.cad").expect("resolve mod");

        assert_eq!(resolved.relative_string(), "points/one/mod.cad");
        assert_eq!(resolved.root_string(), "project");
    }

    #[test]
    fn blocking_prefers_file_form_over_mod_cad() {
        // Both forms exist, so the fixed probe order decides and the file form must win
        let handle = BlockingMemIo::new();
        write(&handle, "points/one.cad");
        write(&handle, "points/one/mod.cad");

        let resolved = blocking(&handle, "project", "points/one.cad").expect("resolve mod");

        assert_eq!(resolved.relative_string(), "points/one.cad");
    }

    #[test]
    fn blocking_accepts_extensionless_stem() {
        let handle = BlockingMemIo::new();
        write(&handle, "points/one.cad");

        // The stem spelling resolves to the same file as the file form spelling
        let resolved = blocking(&handle, "project", "points/one").expect("resolve mod");

        assert_eq!(resolved.relative_string(), "points/one.cad");
    }

    #[test]
    fn blocking_errors_when_neither_exists() {
        let handle = BlockingMemIo::new();

        let outcome = blocking(&handle, "project", "points/one.cad");

        // The error names both probed candidates so the diagnostic points at what was tried
        match outcome {
            Err(FileError::Path { path, message }) => {
                assert_eq!(path, "points/one.cad");
                assert!(message.contains("points/one.cad") && message.contains("mod.cad"));
            },
            other => panic!("expected FileError::Path, got {:?}", other),
        }
    }

    #[test]
    fn memfile_resolves_through_the_store_behind_the_guard() {
        let handle = BlockingMemIo::new();
        write(&handle, "points/one/mod.cad");
        let guard = MemFileGuard::new(&handle);

        // The file was never opened as a buffer, so the store behind the guard answers
        let resolved = memfile_blocking(&guard, "project", "points/one.cad").expect("resolve mod");

        assert_eq!(resolved.relative_string(), "points/one/mod.cad");
    }

    #[test]
    fn memfile_resolves_an_unflushed_buffer() {
        let handle = BlockingMemIo::new();
        let mut guard = MemFileGuard::new(&handle);

        // The module only exists as a live buffer this session - nothing has flushed to the
        // store - and it must still resolve, winning the probe order as the file form
        let path = file_path_str("project", "points/one.cad").expect("construct file path");
        guard.reset_file(&path, "x".to_string());

        let resolved = memfile_blocking(&guard, "project", "points/one.cad").expect("resolve mod");

        assert_eq!(resolved.relative_string(), "points/one.cad");
    }
}
