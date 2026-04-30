pub mod create;
pub mod delete;
pub mod get;
pub mod login;
pub mod logout;

use crate::roles::{NoRoleCheck, Role};
use saps::auth::middleware::attach_refreshed_cookie;
use saps::axum::{
    Router,
    middleware::from_fn,
    routing::{delete as delete_method, get, post},
};
use saps::config::GetConfigVariable;
use saps::dal::connections::{AuthPostGresDescriptor, LivePostGresPool, SqlxPostGresDescriptor};

/// Attaches all user-related views to the router.
///
/// The `attach_refreshed_cookie` layer is applied at the bottom: when the
/// `HeaderToken` extractor rotates a session UUID it stashes the new cookie in
/// the request extensions, and this layer copies it onto the response as
/// `Set-Cookie`. Routes that don't use `HeaderToken` are unaffected — the layer
/// is a no-op when no rotation occurred.
///
/// # Type Parameters
/// * `C` - A type that implements `GetConfigVariable` (e.g. `EnvConfig` or a test config)
pub fn users_factory<C>(app: Router) -> Router
where
    C: GetConfigVariable + Send + Sync + 'static,
{
    app.route(
        "/api/v1/users",
        post(create::create_user_handler::<SqlxPostGresDescriptor<LivePostGresPool>>),
    )
    .route(
        "/api/v1/users/me",
        get(get::get_user_handler::<
            SqlxPostGresDescriptor<LivePostGresPool>,
            C,
            NoRoleCheck,
            Role,
            LivePostGresPool,
        >),
    )
    .route(
        "/api/v1/auth/logout",
        post(
            logout::logout_handler::<
                AuthPostGresDescriptor<LivePostGresPool>,
                C,
                NoRoleCheck,
                Role,
                LivePostGresPool,
            >,
        ),
    )
    .route(
        "/api/v1/users",
        delete_method(
            delete::delete_user_handler::<
                SqlxPostGresDescriptor<LivePostGresPool>,
                C,
                NoRoleCheck,
                Role,
                LivePostGresPool,
            >,
        ),
    )
    .layer(from_fn(attach_refreshed_cookie))
}
