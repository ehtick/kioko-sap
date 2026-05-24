//! Creates a connection pool backed by an embedded PostgreSQL instance.
//!
//! The macro starts a real PostgreSQL process bundled with the binary (via the
//! `postgresql_embedded` crate), creates a database on it, and exposes an
//! `sqlx::PgPool` connected to that database. This is useful for staging
//! deployments or self-contained binaries that should not depend on an external
//! database.
extern crate proc_macro;
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Ident, LitStr, Token, parse::Parse, parse::ParseStream, parse_macro_input};

/// The input args into the embedded database pool.
struct EmbeddedDbPoolArgs {
    /// The name of the connection pool to be referenced throughout the program.
    pool_ident: Ident,
    /// The env variable name holding the database name to create on the embedded server.
    db_name_env: LitStr,
    /// The env variable name holding the maximum number of connections.
    max_conn_env: LitStr,
}

impl Parse for EmbeddedDbPoolArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pool_ident: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let db_name_env: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let max_conn_env: LitStr = input.parse()?;
        Ok(EmbeddedDbPoolArgs {
            pool_ident,
            db_name_env,
            max_conn_env,
        })
    }
}

#[proc_macro]
pub fn define_embedded_pg_pool(input: TokenStream) -> TokenStream {
    let EmbeddedDbPoolArgs {
        pool_ident,
        db_name_env,
        max_conn_env,
    } = parse_macro_input!(input as EmbeddedDbPoolArgs);

    let instance_ident = format_ident!("__{}_EMBEDDED_INSTANCE", pool_ident);
    let init_fn_ident = format_ident!("init_{}", pool_ident.to_string().to_lowercase());

    quote! {
        /// Holds the running embedded PostgreSQL process for the lifetime of the program.
        /// Stored in a `OnceCell` so that the postgres child process is never dropped.
        pub static #instance_ident: saps::tokio::sync::OnceCell<saps::postgresql_embedded::PostgreSQL> =
            saps::tokio::sync::OnceCell::const_new();

        /// The `sqlx` connection pool bound to the embedded PostgreSQL database.
        /// `#init_fn_ident` must be `await`ed at startup before any consumer reads
        /// the pool via `.get()`.
        pub static #pool_ident: saps::tokio::sync::OnceCell<saps::sqlx::postgres::PgPool> =
            saps::tokio::sync::OnceCell::const_new();

        /// Starts the embedded PostgreSQL server, creates the configured database and
        /// initializes the `sqlx::PgPool`. Subsequent calls are no-ops and return the
        /// already initialized pool.
        pub async fn #init_fn_ident() -> &'static saps::sqlx::postgres::PgPool {
            let database_name = std::env::var(#db_name_env)
                .unwrap_or_else(|_| panic!("Get env variable {} for embedded database name", #db_name_env));

            let max_connections = std::env::var(#max_conn_env)
                .unwrap_or_else(|_| "5".to_string())
                .trim()
                .parse::<u32>()
                .unwrap_or_else(|_| panic!("Could not parse {} as max connections", #max_conn_env));

            let postgresql_ref = #instance_ident
                .get_or_init(|| async {
                    let mut postgresql = saps::postgresql_embedded::PostgreSQL::default();
                    postgresql
                        .setup()
                        .await
                        .expect("Failed to set up embedded PostgreSQL");
                    postgresql
                        .start()
                        .await
                        .expect("Failed to start embedded PostgreSQL");

                    if !postgresql
                        .database_exists(&database_name)
                        .await
                        .expect("Failed to check embedded PostgreSQL database existence")
                    {
                        postgresql
                            .create_database(&database_name)
                            .await
                            .expect("Failed to create embedded PostgreSQL database");
                    }

                    postgresql
                })
                .await;

            #pool_ident
                .get_or_init(|| async {
                    let connection_string = postgresql_ref.settings().url(&database_name);
                    saps::sqlx::postgres::PgPoolOptions::new()
                        .max_connections(max_connections)
                        .connect(&connection_string)
                        .await
                        .expect("Failed to connect pool to embedded PostgreSQL")
                })
                .await
        }
    }
    .into()
}
