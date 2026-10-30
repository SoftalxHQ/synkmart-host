//! Document numbers matching cloud Synkmart API (`Merchant#receipt_prefix` + sequential).

use crate::error::{AppError, AppResult};
use sqlx::{Sqlite, SqlitePool, Transaction};

/// First 3 A–Z letters of the merchant name, padded with X (same as API).
pub async fn receipt_prefix(pool: &SqlitePool) -> AppResult<String> {
    let name = crate::db::meta_get(pool, "merchant_name")
        .await?
        .unwrap_or_default();
    Ok(prefix_from_name(&name))
}

pub fn prefix_from_name(name: &str) -> String {
    let letters: String = name
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let letters = if letters.is_empty() {
        "XXX".to_string()
    } else {
        letters
    };
    let mut prefix: String = letters.chars().take(3).collect();
    while prefix.len() < 3 {
        prefix.push('X');
    }
    prefix
}

/// Next `{prefix}######` for a table column, scanning existing values with that prefix.
pub async fn next_ref(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    column: &str,
    prefix: &str,
) -> AppResult<String> {
    let allowed = matches!(
        (table, column),
        ("sales", "receipt_number")
            | ("stock_transfers", "reference_number")
            | ("purchases", "reference_number")
            | ("stock_adjustments", "reference_number")
    );
    if !allowed {
        return Err(AppError::bad("Unsupported reference table."));
    }
    let sql = format!(
        "SELECT {column} FROM {table} WHERE {column} LIKE ? ORDER BY {column} DESC LIMIT 1"
    );
    let like = format!("{prefix}%");
    let last: Option<String> = sqlx::query_scalar(&sql)
        .bind(&like)
        .fetch_optional(&mut **tx)
        .await?;
    let next_number = last
        .as_deref()
        .and_then(|value| value.strip_prefix(prefix))
        .and_then(|digits| digits.parse::<i64>().ok())
        .unwrap_or(0)
        + 1;
    Ok(format!("{prefix}{next_number:06}"))
}

pub async fn next_sale_receipt(tx: &mut Transaction<'_, Sqlite>, pool: &SqlitePool) -> AppResult<String> {
    let prefix = receipt_prefix(pool).await?;
    next_ref(tx, "sales", "receipt_number", &prefix).await
}

pub async fn next_transfer_ref(tx: &mut Transaction<'_, Sqlite>, pool: &SqlitePool) -> AppResult<String> {
    let merchant = receipt_prefix(pool).await?;
    let prefix = format!("TRN{merchant}");
    next_ref(tx, "stock_transfers", "reference_number", &prefix).await
}

pub async fn next_purchase_ref(tx: &mut Transaction<'_, Sqlite>, pool: &SqlitePool) -> AppResult<String> {
    let merchant = receipt_prefix(pool).await?;
    let prefix = format!("PO{merchant}");
    next_ref(tx, "purchases", "reference_number", &prefix).await
}

pub async fn next_adjustment_ref(tx: &mut Transaction<'_, Sqlite>, pool: &SqlitePool) -> AppResult<String> {
    let merchant = receipt_prefix(pool).await?;
    let prefix = format!("ADJ{merchant}");
    next_ref(tx, "stock_adjustments", "reference_number", &prefix).await
}
