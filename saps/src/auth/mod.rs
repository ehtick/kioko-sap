pub mod dal;
pub mod middleware;
pub mod token;
pub mod utils;

// Feature-gated tracing for the auth flow. Enabled by the `auth-tracing` Cargo
// feature; compiles to nothing otherwise so there's no overhead in production
// builds that don't opt in.
#[cfg(feature = "auth-tracing")]
macro_rules! auth_trace {
    ($($arg:tt)*) => { ::tracing::trace!(target: "saps::auth", $($arg)*) };
}
#[cfg(not(feature = "auth-tracing"))]
macro_rules! auth_trace {
    ($($arg:tt)*) => {};
}
pub(crate) use auth_trace;
