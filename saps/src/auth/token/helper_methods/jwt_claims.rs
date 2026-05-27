//! Minimal JWT payload type used internally by [`HeaderToken`].
//!
//! `JwtClaims` holds the two fields actually serialized into the JWT
//! (`unique_id` and `time_expire`). Keeping the encode/decode logic on this
//! type — generic only over the config provider `X` — means the heavy
//! jsonwebtoken machinery compiles once per config provider, instead of
//! once per `(X, Y, R, Z)` combination of [`HeaderToken`].
//!
//! [`HeaderToken`]: crate::auth::token::header_token::HeaderToken

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::{auth::auth_trace, config::GetConfigVariable, errors::saps::SapsError};

/// The JWT payload. Only these two fields cross the wire.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JwtClaims {
    pub unique_id: String,
    pub time_expire: DateTime<Utc>,
}

impl JwtClaims {
    /// Encodes the claims into a signed HS256 JWT string.
    ///
    /// Generic only over the config provider `X` — `HeaderToken::encode`
    /// delegates here so the heavy `jsonwebtoken::encode` machinery is not
    /// monomorphized per `(X, Y, R, Z)`.
    pub fn encode<X: GetConfigVariable>(&self) -> Result<String, SapsError> {
        let key_str = X::get_config_variable("SECRET_KEY".to_string())?;
        let key = EncodingKey::from_secret(key_str.as_ref());
        auth_trace!(
            session_id = %self.unique_id,
            time_expire = %self.time_expire,
            "encoding JWT",
        );
        match encode(&Header::default(), self, &key) {
            Ok(token) => Ok(token),
            Err(error) => {
                auth_trace!(error = %error, "JWT encode failed");
                Err(SapsError::unauthorized(error.to_string()))
            }
        }
    }

    /// Decodes a JWT string into [`JwtClaims`]. `exp` is intentionally not
    /// validated here — expiry checking is handled separately.
    pub fn decode<X: GetConfigVariable>(token: &str) -> Result<Self, SapsError> {
        let key_str = <X>::get_config_variable("SECRET_KEY".to_string())?;
        let key = DecodingKey::from_secret(key_str.as_ref());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.required_spec_claims.remove("exp");

        match decode::<Self>(token, &key, &validation) {
            Ok(token_data) => {
                auth_trace!(
                    session_id = %token_data.claims.unique_id,
                    time_expire = %token_data.claims.time_expire,
                    "decoded JWT",
                );
                Ok(token_data.claims)
            }
            Err(error) => {
                auth_trace!(error = %error, "JWT decode failed");
                Err(SapsError::unauthorized(error.to_string()))
            }
        }
    }
}
