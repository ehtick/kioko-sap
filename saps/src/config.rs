//! Configuration variable providers for the saps authentication system.
//!
//! This module defines how saps retrieves configuration values (such as `SECRET_KEY` and
//! `TOKEN_EXPIRE_MINS`) at runtime. Instead of hard-coding a single source, saps uses
//! the [`GetConfigVariable`] trait so you can swap implementations depending on the context:
//!
//! | Provider | Source | Best for |
//! |----------|--------|----------|
//! | [`EnvConfig`] | `std::env::var` on every call | Simple apps, scripts |
//! | `define_env_config!` | Environment → `OnceLock` (read once at startup) | Production servers |
//! | `define_static_config!` | Hardcoded key/value pairs | Tests, examples |
//!
//! # Required config keys
//!
//! The following keys are used by the auth system and **must** be available from
//! whichever provider you choose:
//!
//! | Key | Type | Used by |
//! |-----|------|---------|
//! | `SECRET_KEY` | String | JWT signing and verification ([`HeaderToken::encode`](crate::auth::token::header_token::HeaderToken::encode) / [`decode`](crate::auth::token::header_token::HeaderToken::decode)) |
//! | `TOKEN_EXPIRE_MINS` | Integer string | Token expiry calculation ([`HeaderToken::new`](crate::auth::token::header_token::HeaderToken::new)) |
//!
//! # Choosing a provider
//!
//! ## `EnvConfig` — direct environment reads
//!
//! The simplest option. Each call to `get_config_variable` reads the environment
//! directly via `std::env::var`. No initialization step is needed, but every request
//! pays the cost of an environment lookup.
//!
//! ```
//! use saps::config::{GetConfigVariable, EnvConfig};
//!
//! // SAFETY: single-threaded doc test.
//! unsafe { std::env::set_var("SECRET_KEY", "my-secret"); }
//!
//! let secret = EnvConfig::get_config_variable("SECRET_KEY").unwrap();
//! assert_eq!(secret, "my-secret");
//! ```
//!
//! ## `define_env_config!` — cached environment reads
//!
//! For production use. Reads each key from the environment **once** during an explicit
//! `init()` call and caches the values in `OnceLock` statics. Subsequent reads are
//! lock-free and allocation-free.
//!
//! ```text
//! // In your application crate:
//! saps::define_env_config!(AppConfig, "SECRET_KEY", "TOKEN_EXPIRE_MINS");
//!
//! // At startup (e.g. in main):
//! AppConfig::init().expect("missing required config");
//!
//! // In handlers — fast, no env lookup:
//! let secret = AppConfig::get_config_variable("SECRET_KEY").unwrap();
//! ```
//!
//! If `init()` is not called before `get_config_variable`, the error message will
//! include `"not initialised — call AppConfig::init() first"`.
//!
//! ## `define_static_config!` — hardcoded values for tests
//!
//! Maps keys to compile-time string literals. Useful in test modules where you don't
//! want to set environment variables.
//!
//! ```text
//! saps::define_static_config!(TestConfig,
//!     "SECRET_KEY" => "test_secret",
//!     "TOKEN_EXPIRE_MINS" => "20"
//! );
//!
//! let key = TestConfig::get_config_variable("SECRET_KEY").unwrap();
//! assert_eq!(key, "test_secret");
//! ```
//!
//! # Implementing your own provider
//!
//! If none of the built-in options fit (e.g. you want to read from a file, Vault, or
//! a remote config service), implement [`GetConfigVariable`] directly:
//!
//! ```
//! use saps::config::GetConfigVariable;
//! use saps::errors::saps::SapsError;
//!
//! struct MyCustomConfig;
//!
//! impl GetConfigVariable for MyCustomConfig {
//!     fn get_config_variable(variable: &str) -> Result<String, SapsError> {
//!         // Your lookup logic here — database, file, remote API, etc.
//!         match variable {
//!             "SECRET_KEY" => Ok("loaded-from-vault".to_string()),
//!             _ => Err(SapsError::unknown(format!("{} not configured", variable))),
//!         }
//!     }
//! }
//! ```
use crate::errors::saps::SapsError;
use std::env;

/// A trait for retrieving configuration values by name.
///
/// This trait abstracts over the source of configuration — environment variables,
/// static maps, remote vaults, etc. The auth system ([`HeaderToken`](crate::auth::token::header_token::HeaderToken))
/// is generic over this trait, so you can plug in any implementation.
///
/// All methods are **static** (no `&self`) because configuration providers are typically
/// stateless — the "instance" is the type itself, and values come from statics, the
/// environment, or external services.
///
/// # Errors
///
/// Implementations should return [`SapsError`] when a requested key is not found or
/// cannot be retrieved. The error message should include the key name to aid debugging.
pub trait GetConfigVariable: Send + Sync {
    /// Retrieves the value of a configuration variable by name.
    ///
    /// # Arguments
    ///
    /// * `variable` — the name of the configuration key to look up (e.g. `"SECRET_KEY"`).
    ///
    /// # Returns
    ///
    /// The value as a `String`, or a [`SapsError`] if the key is not found or retrieval fails.
    fn get_config_variable(variable: &str) -> Result<String, SapsError>;

    /// Retrieves a configuration variable and parses it into `T`.
    ///
    /// For **required** config: a key that is missing errors just like one that
    /// does not parse. Use [`get_config_parsed_or`](GetConfigVariable::get_config_parsed_or)
    /// when the key is optional and has a sensible default.
    ///
    /// The raw value is trimmed before parsing, so surrounding whitespace in an
    /// environment variable does not cause a parse failure.
    ///
    /// # Arguments
    ///
    /// * `variable` — the name of the configuration key to look up (e.g. `"TOKEN_EXPIRE_MINS"`).
    ///
    /// # Returns
    ///
    /// The parsed value, or a [`SapsError`] if the key is not found or its value
    /// does not parse into `T`.
    ///
    /// # Example
    ///
    /// ```
    /// use saps::config::{GetConfigVariable, EnvConfig};
    ///
    /// // SAFETY: single-threaded doc test.
    /// unsafe { std::env::set_var("MAX_RETRIES", "4"); }
    ///
    /// let retries: u32 = EnvConfig::get_config_parsed("MAX_RETRIES").unwrap();
    /// assert_eq!(retries, 4);
    /// ```
    fn get_config_parsed<T: std::str::FromStr>(variable: &str) -> Result<T, SapsError>
    where
        T::Err: std::fmt::Display,
    {
        Self::get_config_variable(variable)?
            .trim()
            .parse::<T>()
            .map_err(|e| SapsError::unknown(format!("{} could not be parsed: {}", variable, e)))
    }

    /// Retrieves a configuration variable and parses it into `T`, falling back
    /// to `default` when the key is not set.
    ///
    /// For **optional** config: only the missing key falls back — a value that
    /// is set but does not parse still errors loudly, because a misconfigured
    /// value should be surfaced rather than silently replaced by the default.
    ///
    /// The raw value is trimmed before parsing, so surrounding whitespace in an
    /// environment variable does not cause a parse failure.
    ///
    /// # Arguments
    ///
    /// * `variable` — the name of the configuration key to look up (e.g. `"MAX_TOKENS"`).
    /// * `default` — the value returned when the key is not set.
    ///
    /// # Returns
    ///
    /// The parsed value, `default` if the key is not found, or a [`SapsError`]
    /// if the key is set but its value does not parse into `T`.
    ///
    /// # Example
    ///
    /// ```
    /// use saps::config::{GetConfigVariable, EnvConfig};
    ///
    /// // UNSET_LIMIT_EXAMPLE is not in the environment, so the default is used.
    /// let limit: u32 = EnvConfig::get_config_parsed_or("UNSET_LIMIT_EXAMPLE", 100).unwrap();
    /// assert_eq!(limit, 100);
    /// ```
    fn get_config_parsed_or<T: std::str::FromStr>(
        variable: &str,
        default: T,
    ) -> Result<T, SapsError>
    where
        T::Err: std::fmt::Display,
    {
        match Self::get_config_variable(variable) {
            // Key is present: it must parse, otherwise it's a config error.
            Ok(raw) => raw.trim().parse::<T>().map_err(|e| {
                SapsError::unknown(format!("{} could not be parsed: {}", variable, e))
            }),
            // Key is absent: use the supplied default.
            Err(_) => Ok(default),
        }
    }
}

/// A configuration provider that reads directly from environment variables.
///
/// Each call to [`get_config_variable`](GetConfigVariable::get_config_variable) invokes
/// `std::env::var` at call time. This is the simplest provider and requires no
/// initialization, but incurs an environment lookup on every call.
///
/// For production servers handling many requests, consider using `define_env_config!`
/// instead, which caches values in `OnceLock` statics after a one-time `init()` call.
///
/// # Example
///
/// ```
/// use saps::config::{GetConfigVariable, EnvConfig};
///
/// // SAFETY: single-threaded doc test.
/// unsafe { std::env::set_var("MY_APP_KEY", "hello"); }
///
/// let value = EnvConfig::get_config_variable("MY_APP_KEY").unwrap();
/// assert_eq!(value, "hello");
/// ```
#[derive(Debug, Clone)]
pub struct EnvConfig;

impl GetConfigVariable for EnvConfig {
    /// Reads the configuration variable from the process environment via `std::env::var`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the environment variable is not set.
    fn get_config_variable(variable: &str) -> Result<String, SapsError> {
        match env::var(variable) {
            Ok(val) => Ok(val),
            Err(_) => Err(SapsError::unknown(format!(
                "{} not found in environment",
                variable
            ))),
        }
    }
}

/// Generates a config struct backed by hardcoded key/value pairs.
///
/// This macro creates a struct that implements [`GetConfigVariable`] by mapping
/// string keys to compile-time string literals. It is primarily intended for
/// **test** modules where you need deterministic config values without setting
/// environment variables.
///
/// # Syntax
///
/// ```text
/// define_static_config!(MyTestConfig,
///     "SECRET_KEY" => "test_secret",
///     "TOKEN_EXPIRE_MINS" => "20"
/// );
/// ```
///
/// You can also use the `DEFAULT` shorthand to generate a `DefaultConfig` struct
/// with a set of common keys pre-filled:
///
/// ```text
/// define_static_config!(DEFAULT);
/// ```
///
/// # Generated code
///
/// - A `pub struct $handle;` (e.g. `MyTestConfig`)
/// - An `impl GetConfigVariable for $handle` that matches on the provided keys
/// - Unknown keys return a [`SapsError`] with the message `"key: {name} was not found"`
#[macro_export]
macro_rules! define_static_config {
    ($handle:ident, $( $key:expr => $value:expr ),*) => {
        #[derive(Clone, Debug)]
        pub struct $handle;
        impl saps::config::GetConfigVariable for $handle {
            fn get_config_variable(variable: &str) -> Result<String, saps::errors::saps::SapsError> {
                match variable {
                    $(
                        $key => Ok($value.to_string()),
                    )*
                    _ => Err(saps::errors::saps::SapsError::unknown(
                        format!("key: {} was not found", variable)
                    ))
                }
            }
        }
    };
    (DEFAULT) => {
        define_static_config!(
            DefaultConfig,
            "FRONTEND_DOMAIN" => "test_domain",
            "MAILCHIMP_API_KEY" => "mock_mailchimp_api",
            "PRODUCTION" => "true",
            "RATE_LIMIT_PERIOD_MINUTES" => "60",
            "RATE_LIMIT" => "5",
            "SECRET_KEY" => "secret",
            "SERVER_TAG" => "test_server"
        );
    };
}

/// Generates a config struct backed by `OnceLock` static variables loaded from the environment.
///
/// Unlike `define_static_config!` which maps keys to hardcoded values, this macro reads
/// values from the environment **once** during an explicit `init()` call and caches them
/// in `OnceLock` statics. Subsequent reads via [`GetConfigVariable::get_config_variable`]
/// are lock-free and allocation-free.
///
/// This is the **recommended provider for production** servers where you want to fail
/// fast at startup if config is missing, and avoid per-request environment lookups.
///
/// # Syntax
///
/// ```text
/// define_env_config!(AppConfig, "SECRET_KEY", "DATABASE_URL", "TOKEN_EXPIRE_MINS");
/// ```
///
/// # Generated code
///
/// - One `OnceLock<String>` static per key
/// - A `pub struct $handle;` (e.g. `AppConfig`)
/// - An `impl $handle { pub fn init() -> Result<(), SapsError> }` that reads each key
///   from the environment and stores it in the corresponding `OnceLock`
/// - An `impl GetConfigVariable for $handle` that reads from the cached `OnceLock` values
///
/// # Usage
///
/// ```text
/// define_env_config!(AppConfig, "SECRET_KEY", "DATABASE_URL");
///
/// // Call once at startup — fails immediately if any key is missing.
/// AppConfig::init().expect("failed to load config");
///
/// // Then use via the trait — fast, no env lookup.
/// let secret = AppConfig::get_config_variable("SECRET_KEY").unwrap();
/// ```
///
/// # Error messages
///
/// | Scenario | Error message |
/// |----------|---------------|
/// | `init()` called but env var missing | `"{KEY} not found in environment"` |
/// | `get_config_variable` before `init()` | `"{KEY} not initialised — call {Handle}::init() first"` |
/// | Unknown key | `"key: {KEY} was not found in {Handle}"` |
#[macro_export]
macro_rules! define_env_config {
    ($handle:ident, $( $key:expr ),* $(,)?) => {
        saps::paste::paste! {
            $(
                static [< __CONFIG_ $handle:upper _ $key:upper >]: std::sync::OnceLock<String> = std::sync::OnceLock::new();
            )*

            #[derive(Clone)]
            pub struct $handle;

            #[allow(dead_code)]
            impl $handle {
                /// Reads each config key from the environment and stores it in a `OnceLock`.
                /// Call this once at startup. Returns an error if any key is missing.
                pub fn init() -> Result<(), saps::errors::saps::SapsError> {
                    $(
                        let val = std::env::var($key).map_err(|_| {
                            saps::errors::saps::SapsError::unknown(
                                format!("{} not found in environment", $key)
                            )
                        })?;
                        [< __CONFIG_ $handle:upper _ $key:upper >].set(val).ok();
                    )*
                    Ok(())
                }
            }

            impl saps::config::GetConfigVariable for $handle {
                fn get_config_variable(variable: &str) -> Result<String, saps::errors::saps::SapsError> {
                    match variable {
                        $(
                            $key => [< __CONFIG_ $handle:upper _ $key:upper >]
                                .get()
                                .cloned()
                                .ok_or_else(|| saps::errors::saps::SapsError::unknown(
                                    format!("{} not initialised — call {}::init() first", $key, stringify!($handle))
                                )),
                        )*
                        _ => Err(saps::errors::saps::SapsError::unknown(
                            format!("key: {} was not found in {}", variable, stringify!($handle))
                        ))
                    }
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {

    use super::*;

    define_env_config!(TestConfig, "TEST_SECRET_KEY", "TEST_DB_URL");

    #[test]
    fn test_init_and_get_config_variable() {
        // Set env vars before init
        unsafe {
            std::env::set_var("TEST_SECRET_KEY", "my_secret");
            std::env::set_var("TEST_DB_URL", "postgres://localhost/test");
        }

        TestConfig::init().expect("init should succeed");

        let secret = TestConfig::get_config_variable("TEST_SECRET_KEY").unwrap();
        assert_eq!(secret, "my_secret");

        let db_url = TestConfig::get_config_variable("TEST_DB_URL").unwrap();
        assert_eq!(db_url, "postgres://localhost/test");
    }

    #[test]
    fn test_get_unknown_key_returns_error() {
        let result = TestConfig::get_config_variable("NONEXISTENT_KEY");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("was not found in TestConfig")
        );
    }

    define_env_config!(UninitConfig, "UNINIT_VAR_XYZ");

    #[test]
    fn test_get_before_init_returns_error() {
        // Don't call init — OnceLock is empty
        let result = UninitConfig::get_config_variable("UNINIT_VAR_XYZ");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("not initialised"));
    }

    define_env_config!(MissingEnvConfig, "THIS_VAR_DOES_NOT_EXIST_12345");

    #[test]
    fn test_init_fails_when_env_var_missing() {
        let result = MissingEnvConfig::init();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("not found in environment")
        );
    }

    define_static_config!(ParsedConfig,
        "MAX_TOKENS" => "8192",
        "TEMPERATURE" => "0.25",
        "ENABLED" => "true",
        "PADDED_NUMBER" => " 42 ",
        "BAD_NUMBER" => "not-a-number"
    );

    #[test]
    fn test_get_config_parsed_returns_parsed_value() {
        let tokens: u32 = ParsedConfig::get_config_parsed("MAX_TOKENS").unwrap();
        assert_eq!(tokens, 8192);
    }

    #[test]
    fn test_get_config_parsed_is_generic_over_target_type() {
        let temperature: f32 = ParsedConfig::get_config_parsed("TEMPERATURE").unwrap();
        assert_eq!(temperature, 0.25);

        let enabled: bool = ParsedConfig::get_config_parsed("ENABLED").unwrap();
        assert!(enabled);
    }

    #[test]
    fn test_get_config_parsed_trims_surrounding_whitespace() {
        let padded: i64 = ParsedConfig::get_config_parsed("PADDED_NUMBER").unwrap();
        assert_eq!(padded, 42);
    }

    #[test]
    fn test_get_config_parsed_errors_when_key_missing() {
        let result = ParsedConfig::get_config_parsed::<u32>("NONEXISTENT_KEY");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("NONEXISTENT_KEY"));
    }

    #[test]
    fn test_get_config_parsed_errors_when_value_unparseable() {
        let result = ParsedConfig::get_config_parsed::<u32>("BAD_NUMBER");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("BAD_NUMBER could not be parsed")
        );
    }

    #[test]
    fn test_get_config_parsed_or_returns_parsed_value_when_set() {
        let tokens: u32 = ParsedConfig::get_config_parsed_or("MAX_TOKENS", 100).unwrap();
        assert_eq!(tokens, 8192);
    }

    #[test]
    fn test_get_config_parsed_or_falls_back_to_default_when_key_missing() {
        let tokens: u32 =
            ParsedConfig::get_config_parsed_or("NONEXISTENT_KEY", 100).unwrap();
        assert_eq!(tokens, 100);
    }

    #[test]
    fn test_get_config_parsed_or_errors_when_value_unparseable() {
        // A set-but-invalid value must surface as an error, not be masked by the default.
        let result = ParsedConfig::get_config_parsed_or::<u32>("BAD_NUMBER", 100);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("BAD_NUMBER could not be parsed")
        );
    }
}
