use crate::{auth::AuthUser, db, error::AppResult, state::AppState};
use chrono::Local;
use serde_json::Value;
use sqlx::SqlitePool;

pub async fn record(
    pool: &SqlitePool,
    action: &str,
    summary: &str,
    user_id: Option<i64>,
    actor: Option<&str>,
    branch_id: Option<i64>,
    metadata: Option<Value>,
) -> AppResult<()> {
    // Store host-local wall time with offset so POS "ago" / day groups match the register clock.
    let created_at = Local::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO activity_logs(action, summary, actor, user_id, branch_id, metadata_json, created_at)
         VALUES(?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(action)
    .bind(summary)
    .bind(actor)
    .bind(user_id)
    .bind(branch_id)
    .bind(metadata.map(|v| v.to_string()))
    .bind(&created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Best-effort activity write — never fails the surrounding request.
pub async fn log(
    state: &AppState,
    auth: &AuthUser,
    action: &str,
    summary: String,
    metadata: Option<Value>,
) {
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    let _ = record(
        &state.pool,
        action,
        &summary,
        Some(auth.user.id),
        Some(&auth.user.name),
        branch_id,
        metadata,
    )
    .await;
}

pub async fn log_user(
    state: &AppState,
    user_id: i64,
    actor: &str,
    action: &str,
    summary: String,
    metadata: Option<Value>,
) {
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    let _ = record(
        &state.pool,
        action,
        &summary,
        Some(user_id),
        Some(actor),
        branch_id,
        metadata,
    )
    .await;
}
