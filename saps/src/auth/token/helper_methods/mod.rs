//! Helper functions that are detached from the HeaderToken to reduce monomorphization
//! and speed up compiler time.
pub mod extract_bearer_token;
pub mod extract_token_from_cookies;
pub mod extract_token_from_header;
pub mod jwt_claims;
pub mod parse_cookie_value;
pub mod run_auth_extraction;
pub mod meta;
