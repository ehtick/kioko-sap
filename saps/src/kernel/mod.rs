//! Shared data structures with no runtime behaviour of their own.
//!
//! Everything in here is plain data — structs and enums that travel between
//! layers of an application (over an actor message, a websocket frame, or a
//! database row) and carry no IO, no async runtime, and no server dependency.
//! That is why the module sits at the baseline of the crate rather than behind
//! a feature: a wasm frontend and a native server both need to name the same
//! types, and neither should have to pull the other's dependencies in to do it.
//!
//! Types that need to cross into JavaScript carry a `#[wasm_bindgen]` export
//! behind the `wasm` feature, so a browser build gets the JS class and a server
//! build does not pay for `wasm-bindgen`.
pub mod transaction;
