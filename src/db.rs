use anyhow::{Context, Result};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{fs, path::Path, str::FromStr};

pub async fn connect(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create host data directory")?;
    }

    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("connect sqlite")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;

    Ok(pool)
}

pub async fn meta_get(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM store_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub async fn meta_set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO store_meta(key, value) VALUES(?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// Same shape as cloud `Merchant#generate_public_id` — `SKM` + 6 hex digits.
pub fn generate_merchant_code() -> String {
    use rand::Rng;
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut rng = rand::rng();
    let mut out = String::from("SKM");
    for _ in 0..6 {
        out.push(HEX[rng.random_range(0..16)] as char);
    }
    out
}

pub async fn ensure_merchant_code(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    if let Some(code) = meta_get(pool, "merchant_code").await? {
        if !code.trim().is_empty() {
            return Ok(code);
        }
    }
    let code = generate_merchant_code();
    meta_set(pool, "merchant_code", &code).await?;
    Ok(code)
}

/// POS / sales sell only from an active store location (never warehouse).
pub async fn store_location_id(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    if let Some(id) = meta_get(pool, "store_location_id")
        .await?
        .and_then(|v| v.parse().ok())
    {
        return Ok(id);
    }
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM stock_locations
         WHERE kind = 'store' AND IFNULL(status, 'active') = 'active'
         ORDER BY id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(id);
    }
    Ok(meta_get(pool, "stock_location_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(2))
}

pub async fn warehouse_location_id(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    if let Some(id) = meta_get(pool, "warehouse_location_id")
        .await?
        .and_then(|v| v.parse().ok())
    {
        return Ok(id);
    }
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM stock_locations
         WHERE kind = 'warehouse' AND IFNULL(status, 'active') = 'active'
         ORDER BY id LIMIT 1",
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(id);
    }
    Ok(1)
}
