use crate::error::AppResult;
use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

pub async fn enqueue(
    tx: &mut Transaction<'_, Sqlite>,
    event_type: &str,
    payload: Value,
) -> AppResult<()> {
    let event_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO outbox(event_id, event_type, payload_json) VALUES(?, ?, ?)")
        .bind(&event_id)
        .bind(event_type)
        .bind(payload.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
