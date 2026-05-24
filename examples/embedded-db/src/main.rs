//! Boots an embedded PostgreSQL instance via the framework-integrated
//! [`LiveEmbeddedPostgresPool`] handle and runs a handful of raw `sqlx` queries
//! against it.
//!
//! Run with:
//!     cargo run -p embedded-db
//!
//! Override defaults via env vars `EMBEDDED_DATABASE_NAME` and `DB_MAX_CONNECTIONS`.

use saps::dal::connections::YieldPostGresPool;
use saps::dal::embedded_connections::{
    LiveEmbeddedPostgresPool, init_live_embedded_postgres_pool,
};

fn ensure_env(key: &str, default: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: single-threaded program start, before any thread reads env.
        unsafe { std::env::set_var(key, default) };
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ensure_env("EMBEDDED_DATABASE_NAME", "demo");
    ensure_env("DB_MAX_CONNECTIONS", "5");

    println!("[1/5] Booting embedded PostgreSQL (may download binaries on first run)...");
    init_live_embedded_postgres_pool().await;
    println!("       embedded PostgreSQL ready.");

    // Now the framework-wide YieldPostGresPool handle is usable.
    let pool = LiveEmbeddedPostgresPool::yield_pool();

    println!("[2/5] Creating `notes` table...");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notes (
            id   SERIAL PRIMARY KEY,
            body TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    println!("[3/5] Truncating any prior rows...");
    sqlx::query("TRUNCATE TABLE notes RESTART IDENTITY")
        .execute(pool)
        .await?;

    println!("[4/5] Inserting three rows...");
    sqlx::query("INSERT INTO notes (body) VALUES ($1), ($2), ($3)")
        .bind("first note")
        .bind("second note")
        .bind("third note")
        .execute(pool)
        .await?;

    println!("[5/5] Reading back...");
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes")
        .fetch_one(pool)
        .await?;
    println!("       row count = {count}");

    let rows: Vec<(i32, String)> = sqlx::query_as("SELECT id, body FROM notes ORDER BY id")
        .fetch_all(pool)
        .await?;
    for (id, body) in rows {
        println!("       - {id}: {body}");
    }

    println!("Done.");
    Ok(())
}
