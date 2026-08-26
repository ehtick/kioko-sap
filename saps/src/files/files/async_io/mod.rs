#[cfg(all(feature = "files-indexed-db", target_arch = "wasm32"))]
pub mod indexed_db;
pub mod mem;
