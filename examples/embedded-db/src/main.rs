//! Boots an embedded PostgreSQL instance via the `define_embedded_pg_pool!`
//! macro and runs a handful of raw `sqlx` queries against it.
//!
//! Run with:
//!     cargo run -p embedded-db
//!
//! Override defaults via env vars `EMBEDDED_DB_NAME` and `DB_MAX_CONNECTIONS`.

saps_db_embedded_pool_macro::define_embedded_pg_pool!(
    EMBEDDED_POOL,
    "EMBEDDED_DB_NAME",
    "DB_MAX_CONNECTIONS"
);

fn ensure_env(key: &str, default: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: single-threaded program start, before any thread reads env.
        unsafe { std::env::set_var(key, default) };
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ensure_env("EMBEDDED_DB_NAME", "demo");
    ensure_env("DB_MAX_CONNECTIONS", "5");

    println!("[1/5] Booting embedded PostgreSQL (may download binaries on first run)...");
    let pool = init_embedded_pool().await;
    println!("       embedded PostgreSQL ready.");

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
