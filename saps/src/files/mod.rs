//! File and folder operations, from the typed path down to the storage engine.
//!
//! The module answers one question in one place: how does this application read,
//! write, move, copy and delete a file, wherever that file actually lives. A
//! caller names a path, picks an engine, and calls an [`api`] function; nothing
//! above this layer needs to know whether the bytes end up on a disk, in a
//! key-value store, in memory, or in the browser's IndexedDB.
//!
//! # Layout
//!
//! The layers stack, each one only knowing about the ones below it:
//!
//! | Module | Role |
//! |---|---|
//! | [`api`] | The front door — one module per operation (read, write, copy, …) |
//! | [`adapters`] | Ergonomic constructors for the typed paths |
//! | [`files`], [`folders`] | Per-engine implementations of the IO traits |
//! | [`engines`] | The storage backends themselves |
//! | [`io`] | The traits the engines implement |
//! | [`paths`] | Typed paths — a file path and a folder path are different types |
//!
//! # Variants rather than a runtime switch
//!
//! Every [`api`] operation is a module holding the same operation written for
//! each execution context — `blocking`, `asynchronous`, and the `memfile_*`
//! forms that go through a live in-memory buffer. They are separate functions
//! rather than one function branching at runtime because the contexts have
//! genuinely different signatures: an async engine must be awaited, and a
//! buffered write has to reach the open buffer rather than the store behind it.
//!
//! # Engines
//!
//! The engine is a value the caller passes in, so the same operation runs
//! against a real disk in production and an in-memory double in a test without
//! the operation knowing. Which engines exist depends on the features enabled:
//! disk and in-memory are always present, `files-kv` adds the redb-backed store,
//! and `files-indexed-db` adds the browser one (that engine is additionally
//! gated on `target_arch = "wasm32"`, since it drives real IndexedDB).
//!
//! # Collaborative buffers
//!
//! [`files::mem_files`] holds the editable buffers. They are backed by a CRDT
//! document, so two replicas that edit the same file concurrently converge
//! rather than clobbering each other, and an edit batch travels as a
//! [`crate::kernel::transaction::Transaction`].
pub mod adapters;
pub mod api;
pub mod engines;
pub mod files;
pub mod folders;
pub mod io;
pub mod paths;
