//! The shared `yrs` transaction functions for the mem-file buffers.
//!
//! These are the document operations lifted out of the server/browser `MemFile` so both the
//! blocking and async wrappers can call the same logic. Every function takes plain mutable
//! references to the document and its text handle rather than `&mut self`, and none of them
//! save or count edits: the batching policy (`record_single_edit` / `save` / flush on drop)
//! is the wrapper's job, layered on top of these primitives. What lives here is purely the
//! text manipulation and the collaboration seam.

use crate::errors::file::FileError;
use crate::kernel::transaction::CursorIndex;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, TextRef, Transact, Update};

use crate::files::paths::{FilePath, Path};

/// Inserts a single character at a cursor position. Per key stroke.
///
/// The common typing path. `contents` is the buffer's current text, used only to resolve
/// the line/column `position` into a byte offset, so the caller passes the text it already
/// materialised rather than this recomputing it. The edit is not saved or counted here — the
/// wrapper decides when a run of these is worth flushing.
///
/// # Arguments
/// - `contents`: The buffer's current text, for resolving `position`.
/// - `text`: The document's root text handle to insert into.
/// - `doc`: The document the insert is transacted against.
/// - `position`: Where to insert, as a line and column.
/// - `character`: The character to insert.
///
/// # Returns
/// `Ok(())` once the character is inserted.
pub fn insert_char(
    contents: &String,
    text: &mut TextRef,
    doc: &mut Doc,
    position: &CursorIndex,
    character: char,
) -> Result<(), FileError> {
    let offset = line_col_to_byte(contents, position);
    {
        let mut buffer = [0u8; 4];
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, offset, character.encode_utf8(&mut buffer));
    }
    Ok(())
}

/// Deletes the single character at a cursor position. Per key stroke.
///
/// Like `insert_char` this is the common path and does not save or count the edit. A
/// position at or past the end of the buffer is a no-op rather than a panic, because the
/// span to remove clamps to the characters actually available.
///
/// # Arguments
/// - `contents`: The buffer's current text, for resolving `position`.
/// - `text`: The document's root text handle to delete from.
/// - `doc`: The document the delete is transacted against.
/// - `position`: The position of the character to delete, as a line and column.
///
/// # Returns
/// `Ok(())` once the character is removed (or nothing, if the position is past the end).
pub fn delete_char(
    contents: &String,
    text: &mut TextRef,
    doc: &mut Doc,
    position: &CursorIndex,
) -> Result<(), FileError> {
    let (offset, length) = byte_span(contents, position, 1);
    if length > 0 {
        let mut txn = doc.transact_mut();
        text.remove_range(&mut txn, offset, length);
    }
    Ok(())
}

/// Deletes a run of characters starting at a cursor position.
///
/// The "highlighted a range and deleted it" path. The run is clamped to the characters
/// actually available from `position`, so an over-long `delta` removes to the end rather
/// than panicking. As with the other utils, saving is left to the caller.
///
/// # Arguments
/// - `contents`: The buffer's current text, for resolving `position` and the run length.
/// - `text`: The document's root text handle to delete from.
/// - `doc`: The document the delete is transacted against.
/// - `position`: The start of the range to delete, as a line and column.
/// - `delta`: The number of characters to remove from `position` onwards.
///
/// # Returns
/// `Ok(())` once the run is removed (or nothing, if there is nothing to remove).
pub fn delete_range(
    contents: &String,
    text: &mut TextRef,
    doc: &mut Doc,
    position: &CursorIndex,
    delta: usize,
) -> Result<(), FileError> {
    let (offset, removable) = byte_span(contents, position, delta);
    if removable > 0 {
        let mut txn = doc.transact_mut();
        text.remove_range(&mut txn, offset, removable);
    }
    Ok(())
}

/// Inserts a run of text at a cursor position.
///
/// The batched / paste path. `contents` resolves the line/column `position` into a byte
/// offset; the text is then inserted there. Saving is left to the caller.
///
/// # Arguments
/// - `contents`: The buffer's current text, for resolving `position`.
/// - `text`: The document's root text handle to insert into.
/// - `doc`: The document the insert is transacted against.
/// - `position`: Where to insert the text, as a line and column.
/// - `data`: The text to insert.
///
/// # Returns
/// `Ok(())` once the text is inserted.
pub fn insert_text(
    contents: &String,
    text: &mut TextRef,
    doc: &mut Doc,
    position: &CursorIndex,
    data: &str,
) -> Result<(), FileError> {
    let offset = line_col_to_byte(contents, position);
    {
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, offset, data);
    }
    Ok(())
}

/// The current text of the buffer, including any unsaved edits.
///
/// This is the materialised view the LSP / compiler reads and the exact bytes a save writes
/// to the backend. It is also what the mutation utils take as their `contents` argument to
/// resolve line and column positions.
///
/// # Arguments
/// - `text`: The document's root text handle to read.
/// - `doc`: The document to read a transaction from.
///
/// # Returns
/// The buffer's full text.
pub fn contents(text: &TextRef, doc: &Doc) -> String {
    text.get_string(&doc.transact())
}

// MARK: - Collaboration seam (the network boundary)

/// Encodes the whole document as a `yrs` state update.
///
/// This is what a joining replica is seeded with, and what you persist as a durable CRDT
/// snapshot so a reloaded session can rebuild the document (not just the flat text) and keep
/// merging.
///
/// # Arguments
/// - `doc`: The document to encode.
///
/// # Returns
/// The document's full state as a v1 update.
pub fn encode_state(doc: &Doc) -> Vec<u8> {
    doc.transact().encode_state_as_update_v1(&StateVector::default())
}

/// Encodes this replica's state vector — a compact summary of which edits it already has.
///
/// A joining replica sends this to the origin, which replies with [`encode_diff`] carrying
/// only the edits this replica is missing, so catch-up does not replay the whole history.
///
/// # Arguments
/// - `doc`: The document whose state vector is summarised.
///
/// # Returns
/// The state vector as a v1 blob.
pub fn state_vector(doc: &Doc) -> Vec<u8> {
    doc.transact().state_vector().encode_v1()
}

/// Encodes only the edits missing from a peer with the given `state_vector`.
///
/// The other half of the joining handshake: given a peer's [`state_vector`], produce the
/// minimal update that brings it up to date. `path` labels any decode failure with the file
/// it concerns.
///
/// # Arguments
/// - `doc`: The document to diff against the peer.
/// - `path`: The logical path, used only to tag a decode error.
/// - `state_vector`: The peer's state vector as produced by [`state_vector`].
///
/// # Returns
/// The minimal update the peer is missing, or a `FileError::MemFile` if the state vector
/// cannot be decoded.
pub fn encode_diff(
    doc: &Doc,
    path: &Path<FilePath>,
    state_vector: &[u8],
) -> Result<Vec<u8>, FileError> {
    let remote = StateVector::decode_v1(state_vector)
        .map_err(|error| FileError::MemFile { path: path.into(), message: error.to_string() })?;
    Ok(doc.transact().encode_diff_v1(&remote))
}

/// Applies a `yrs` update from another replica (for example one fanned out by the relay) to
/// the document.
///
/// Updates are idempotent and commutative, so applying the same update twice or applying
/// updates out of order still converges. The document is not saved here — the caller decides
/// when a run of relayed edits is worth a flush. `path` labels any decode or apply failure
/// with the file it concerns.
///
/// # Arguments
/// - `doc`: The document to apply the update to.
/// - `path`: The logical path, used only to tag a decode or apply error.
/// - `update`: A `yrs` v1 update blob, as produced by [`encode_diff`] or another replica's
///   update observer.
///
/// # Returns
/// `Ok(())` once applied, or a `FileError::MemFile` if the update cannot be decoded or
/// applied.
pub fn apply_update(doc: &mut Doc, path: &Path<FilePath>, update: &[u8]) -> Result<(), FileError> {
    let update = Update::decode_v1(update)
        .map_err(|error| FileError::MemFile { path: path.into(), message: error.to_string() })?;
    let mut txn = doc.transact_mut();
    txn.apply_update(update)
        .map_err(|error| FileError::MemFile { path: path.into(), message: error.to_string() })?;
    Ok(())
}

// MARK: - Offset helpers

/// Resolves a line and column position into a flat UTF-8 byte offset into `text`.
///
/// `yrs` addresses text by a single index in its default byte-offset mode, whereas the
/// editor thinks in lines and columns counted in characters. This finds the byte where
/// `position.line` starts, then advances `position.col` characters along that line summing
/// their UTF-8 widths, and clamps to the end of the buffer so an out-of-range position lands
/// at the end rather than panicking — the byte-accurate equivalent of `ropey`'s
/// `line_to_char(line) + col`. Counting columns in characters but addressing `yrs` in bytes
/// is what keeps multi-byte text correct.
///
/// # Arguments
/// - `text`: The text to resolve the position against.
/// - `position`: The line and column to resolve.
///
/// # Returns
/// The byte offset of `position`, clamped to the end of `text`.
fn line_col_to_byte(text: &str, position: &CursorIndex) -> u32 {
    let line_start = line_start_byte(text, position.line);
    let mut byte = line_start;
    for character in text[line_start..].chars().take(position.col) {
        byte += character.len_utf8();
    }
    byte.min(text.len()) as u32
}

/// Resolves a position and a length in characters into a `(byte offset, byte length)` span
/// for `yrs`.
///
/// The offset is `position` in bytes (as [`line_col_to_byte`]); the length is the UTF-8
/// width of the next `char_length` characters from there. Advancing by characters naturally
/// stops at the end of the buffer, so an over-long `char_length` yields the span to the end
/// rather than running past it.
///
/// # Arguments
/// - `text`: The text to resolve the span against.
/// - `position`: The start of the span, as a line and column.
/// - `char_length`: How many characters the span should cover.
///
/// # Returns
/// The `(byte offset, byte length)` of the span, clamped to the end of `text`.
fn byte_span(text: &str, position: &CursorIndex, char_length: usize) -> (u32, u32) {
    let start = line_col_to_byte(text, position) as usize;
    let mut end = start;
    for character in text[start..].chars().take(char_length) {
        end += character.len_utf8();
    }
    (start as u32, (end - start) as u32)
}

/// The UTF-8 byte index where zero-based `line` begins.
///
/// Line zero starts at byte zero; each later line starts just past the newline that ends the
/// line before it. A `line` past the end of the text resolves to the end of the text.
///
/// # Arguments
/// - `text`: The text to scan for line boundaries.
/// - `line`: The zero-based line whose start is wanted.
///
/// # Returns
/// The byte index where `line` begins, or the end of `text` if `line` is past the end.
pub fn line_start_byte(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let mut newlines_seen = 0;
    for (index, character) in text.char_indices() {
        if character == '\n' {
            newlines_seen += 1;
            if newlines_seen == line {
                return index + character.len_utf8();
            }
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    // Each test builds a raw `yrs` document seeded with known text, drives one util against
    // it, and reads the result back through `contents`. The utils never save, so nothing here
    // touches a backend; the collaboration-seam tests wire two documents together directly.

    use super::*;
    use crate::files::files::mem_files::constants::CONTENT_FIELD;

    const START: &str = "the cat sat on the mat";

    /// Builds a `(line, col)` cursor.
    fn cursor(line: usize, col: usize) -> CursorIndex {
        CursorIndex { line, col }
    }

    /// A throwaway file path for tagging seam errors in the tests.
    fn path() -> Path<FilePath> {
        Path::<FilePath>::new("f.cad").unwrap()
    }

    /// Builds a fresh document seeded with `text` under the root content field, returning it
    /// alongside its text handle.
    fn seeded(text: &str) -> (Doc, TextRef) {
        let doc = Doc::new();
        let handle = doc.get_or_insert_text(CONTENT_FIELD);
        if !text.is_empty() {
            let mut txn = doc.transact_mut();
            handle.insert(&mut txn, 0, text);
        }
        (doc, handle)
    }

    // MARK: - Mutations

    #[test]
    fn insert_char_inserts_at_position() {
        let (mut doc, mut text) = seeded(START);
        let current = contents(&text, &doc);

        insert_char(&current, &mut text, &mut doc, &cursor(0, 0), 'X').unwrap();
        assert_eq!(contents(&text, &doc), "Xthe cat sat on the mat");
    }

    #[test]
    fn delete_char_removes_character_at_position() {
        let (mut doc, mut text) = seeded(START);
        let current = contents(&text, &doc);

        delete_char(&current, &mut text, &mut doc, &cursor(0, 0)).unwrap();
        assert_eq!(contents(&text, &doc), "he cat sat on the mat");
    }

    #[test]
    fn delete_char_past_end_is_a_noop() {
        let (mut doc, mut text) = seeded("hi");
        let current = contents(&text, &doc);

        delete_char(&current, &mut text, &mut doc, &cursor(0, 10)).unwrap();
        assert_eq!(contents(&text, &doc), "hi");
    }

    #[test]
    fn delete_range_removes_a_run() {
        let (mut doc, mut text) = seeded(START);
        let current = contents(&text, &doc);

        delete_range(&current, &mut text, &mut doc, &cursor(0, 0), 4).unwrap();
        assert_eq!(contents(&text, &doc), "cat sat on the mat");
    }

    #[test]
    fn delete_range_clamps_to_end() {
        let (mut doc, mut text) = seeded(START);
        let current = contents(&text, &doc);

        // Far more than remain from column 18; removes to the end rather than panicking.
        delete_range(&current, &mut text, &mut doc, &cursor(0, 18), 999).unwrap();
        assert_eq!(contents(&text, &doc), "the cat sat on the");
    }

    #[test]
    fn insert_text_inserts_a_run() {
        let (mut doc, mut text) = seeded(START);
        let current = contents(&text, &doc);

        insert_text(&current, &mut text, &mut doc, &cursor(0, 4), "big ").unwrap();
        assert_eq!(contents(&text, &doc), "the big cat sat on the mat");
    }

    #[test]
    fn edits_resolve_against_line_and_column() {
        let (mut doc, mut text) = seeded("first line\nsecond line");
        let current = contents(&text, &doc);

        // Column three on line one is before the 'o' of "second".
        insert_text(&current, &mut text, &mut doc, &cursor(1, 3), "X").unwrap();
        assert_eq!(contents(&text, &doc), "first line\nsecXond line");
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        // 'é' is two UTF-8 bytes, so column four is the end of "café"; the byte offset must
        // account for that width or the insert lands in the wrong place.
        let (mut doc, mut text) = seeded("café");
        let current = contents(&text, &doc);

        insert_char(&current, &mut text, &mut doc, &cursor(0, 4), '!').unwrap();
        assert_eq!(contents(&text, &doc), "café!");
    }

    // MARK: - Collaboration seam

    #[test]
    fn encode_state_then_apply_reproduces_document() {
        let (origin_doc, _origin_text) = seeded(START);
        let state = encode_state(&origin_doc);

        let (mut joined_doc, joined_text) = seeded("");
        apply_update(&mut joined_doc, &path(), &state).unwrap();
        assert_eq!(contents(&joined_text, &joined_doc), START);
    }

    #[test]
    fn state_vector_diff_apply_converges() {
        let (origin_doc, _origin_text) = seeded(START);
        let (mut joiner_doc, joiner_text) = seeded("");

        // Joiner asks for what it lacks; origin replies with just that diff.
        let summary = state_vector(&joiner_doc);
        let diff = encode_diff(&origin_doc, &path(), &summary).unwrap();
        apply_update(&mut joiner_doc, &path(), &diff).unwrap();

        assert_eq!(contents(&joiner_text, &joiner_doc), START);
    }

    #[test]
    fn apply_update_rejects_malformed_update() {
        let (mut doc, _text) = seeded("");
        let result = apply_update(&mut doc, &path(), &[0xff, 0x00, 0x13, 0x37]);

        assert!(matches!(result, Err(FileError::MemFile { .. })));
    }

    // MARK: - Offset helpers

    #[test]
    fn line_start_byte_finds_line_offsets() {
        let text = "first\nsecond\nthird";
        assert_eq!(line_start_byte(text, 0), 0);
        assert_eq!(line_start_byte(text, 1), 6); // just past "first\n"
        assert_eq!(line_start_byte(text, 2), 13); // just past "second\n"
        assert_eq!(line_start_byte(text, 9), text.len()); // past the end clamps to the end
    }

    #[test]
    fn byte_span_covers_the_requested_characters() {
        // From byte 0, three characters of "café" are 'c','a','f' — three bytes.
        assert_eq!(byte_span("café", &cursor(0, 0), 3), (0, 3));
        // Four characters reach 'é', adding its two bytes for five in total.
        assert_eq!(byte_span("café", &cursor(0, 0), 4), (0, 5));
        // An over-long request clamps to the end.
        assert_eq!(byte_span("café", &cursor(0, 0), 99), (0, 5));
    }
}
