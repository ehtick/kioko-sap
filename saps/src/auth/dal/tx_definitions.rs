use super::model::AuthSession;
use crate::auth::token::checks::UserRole;
use crate::define_dal_transactions;
use serde_json::Value;

define_dal_transactions!(
    CreateAuthSession => create_auth_session[U: UserRole](session: AuthSession<U>) -> AuthSession<U>,
    PingAuthSession => ping_auth_session[U: UserRole](minutes: i32, session_id: &str) -> Option<AuthSession<U>>,
    DeleteAuthSession => delete_auth_session(session_id: uuid::Uuid) -> bool,
    GetAllAuthSessions => get_all_auth_sessions[U: UserRole]() -> Vec<AuthSession<U>>,
    GetAuthSession => get_auth_session[U: UserRole](session_id: &str) -> Option<AuthSession<U>>,
    GetAuthSessionStrict => get_auth_session_strict[U: UserRole](session_id: &str) -> AuthSession<U>,
    UpdateAuthSessionMeta => update_auth_session_meta(session_id: &str, meta: Value) -> (),
    UpsertAuthSessionMetaKey => upsert_auth_session_meta_key[U: UserRole](session_id: &str, key: &str, value: Value) -> Option<AuthSession<U>>,
    DeleteAuthSessionMetaKey => delete_auth_session_meta_key[U: UserRole](session_id: &str, key: &str) -> Option<AuthSession<U>>,
    GetAuthSessionsByMetaKey => get_auth_sessions_by_meta_key[U: UserRole](key: &str, value: Value) -> Vec<AuthSession<U>>,
    GetAuthSessionByMetaKey => get_auth_session_by_meta_key[U: UserRole](key: &str, value: Value) -> Option<AuthSession<U>>,
    GetAuthSessionByMetaKeyStrict => get_auth_session_by_meta_key_strict[U: UserRole](key: &str, value: Value) -> AuthSession<U>,
    DeleteAuthSessionsByMetaKey => delete_auth_sessions_by_meta_key(key: &str, value: Value) -> u64,
    UpsertAuthSessionsMetaKeyByMeta => upsert_auth_sessions_meta_key_by_meta(match_key: &str, match_value: Value, upsert_key: &str, upsert_value: Value) -> u64
);
