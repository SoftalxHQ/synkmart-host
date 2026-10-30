use crate::{
    activity,
    auth::AuthUser,
    db,
    error::{AppError, AppResult},
    money::{de_amount, de_opt_amount, format_money, parse_money},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;

#[derive(Debug, FromRow)]
struct RegisterRow {
    id: i64,
    user_id: i64,
    status: String,
    opening_float: String,
    closing_counted: Option<String>,
    expected_cash: Option<String>,
    variance: Option<String>,
    notes: Option<String>,
    opened_at: String,
    closed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenBody {
    #[serde(default, deserialize_with = "de_opt_amount")]
    pub opening_float: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloseBody {
    #[serde(deserialize_with = "de_amount")]
    pub closing_counted: String,
    #[serde(default)]
    pub notes: Option<String>,
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("sales")?;
    let rows = sqlx::query_as::<_, RegisterRow>(
        "SELECT id, user_id, status, opening_float, closing_counted, expected_cash, variance, notes, opened_at, closed_at
         FROM register_sessions
         WHERE user_id = ?
         ORDER BY opened_at DESC
         LIMIT 50",
    )
    .bind(auth.user.id)
    .fetch_all(&state.pool)
    .await?;

    let current = rows.iter().find(|r| r.status == "open").map(session_json);
    let register_sessions: Vec<Value> = rows.iter().map(session_json).collect();
    Ok(Json(json!({
        "register_sessions": register_sessions,
        "current": current,
    })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<OpenBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("sales")?;
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM register_sessions WHERE user_id = ? AND status = 'open' LIMIT 1",
    )
    .bind(auth.user.id)
    .fetch_optional(&state.pool)
    .await?;
    if existing.is_some() {
        return Err(AppError::bad("This cashier already has an open register."));
    }

    let float = parse_money(body.opening_float.as_deref().unwrap_or("0"))?;
    if float < 0.0 {
        return Err(AppError::bad("Opening float cannot be negative."));
    }
    let branch_id = meta_i64(&state, "branch_id").await?.unwrap_or(1);
    let location_id = meta_i64(&state, "stock_location_id").await?;
    let opened_at = Utc::now().to_rfc3339();
    let float_s = format_money(float);

    let result = sqlx::query(
        "INSERT INTO register_sessions(user_id, branch_id, stock_location_id, status, opening_float, expected_cash, opened_at)
         VALUES(?, ?, ?, 'open', ?, ?, ?)",
    )
    .bind(auth.user.id)
    .bind(branch_id)
    .bind(location_id)
    .bind(&float_s)
    .bind(&float_s)
    .bind(&opened_at)
    .execute(&state.pool)
    .await?;

    let id = result.last_insert_rowid();
    let row = sqlx::query_as::<_, RegisterRow>(
        "SELECT id, user_id, status, opening_float, closing_counted, expected_cash, variance, notes, opened_at, closed_at
         FROM register_sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    activity::log(
        &state,
        &auth,
        "register.opened",
        format!(
            "{} opened the register with a cash float of {}",
            auth.user.name, float_s
        ),
        Some(json!({ "opening_float": float_s, "cashier": auth.user.name })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "register_session": session_json(&row) })),
    ))
}

pub async fn close(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<CloseBody>,
) -> AppResult<Json<Value>> {
    auth.require("sales")?;
    let row = sqlx::query_as::<_, RegisterRow>(
        "SELECT id, user_id, status, opening_float, closing_counted, expected_cash, variance, notes, opened_at, closed_at
         FROM register_sessions WHERE id = ? AND user_id = ?",
    )
    .bind(id)
    .bind(auth.user.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Register session not found.".into()))?;

    if row.status != "open" {
        return Err(AppError::bad("This register is already closed."));
    }

    let counted = parse_money(&body.closing_counted)?;
    let opening = parse_money(&row.opening_float)?;
    let cash_sales: f64 = sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(SUM(sp.amount), '0')
         FROM sale_payments sp
         JOIN sales s ON s.id = sp.sale_id
         JOIN register_sessions r ON r.user_id = s.user_id AND r.status = 'open' AND r.id = ?
         WHERE sp.method = 'cash' AND s.created_at >= r.opened_at",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?
    .parse()
    .unwrap_or(0.0);

    let expected = opening + cash_sales;
    let variance = counted - expected;
    let closed_at = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE register_sessions
         SET status = 'closed', closing_counted = ?, expected_cash = ?, variance = ?, notes = ?, closed_at = ?
         WHERE id = ?",
    )
    .bind(format_money(counted))
    .bind(format_money(expected))
    .bind(format_money(variance))
    .bind(body.notes)
    .bind(&closed_at)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let updated = sqlx::query_as::<_, RegisterRow>(
        "SELECT id, user_id, status, opening_float, closing_counted, expected_cash, variance, notes, opened_at, closed_at
         FROM register_sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    activity::log(
        &state,
        &auth,
        "register.closed",
        format!(
            "{} closed the register: counted {} against expected {} (variance {})",
            auth.user.name,
            format_money(counted),
            format_money(expected),
            format_money(variance)
        ),
        Some(json!({
            "closing_counted": format_money(counted),
            "expected_cash": format_money(expected),
            "variance": format_money(variance),
            "cashier": auth.user.name,
        })),
    )
    .await;

    Ok(Json(json!({ "register_session": session_json(&updated) })))
}

fn session_json(row: &RegisterRow) -> Value {
    json!({
        "id": row.id,
        "user_id": row.user_id,
        "status": row.status,
        "opening_float": row.opening_float,
        "closing_counted": row.closing_counted,
        "expected_cash": row.expected_cash.clone().unwrap_or_else(|| row.opening_float.clone()),
        "variance": row.variance,
        "notes": row.notes,
        "opened_at": row.opened_at,
        "closed_at": row.closed_at,
    })
}

async fn meta_i64(state: &AppState, key: &str) -> AppResult<Option<i64>> {
    Ok(db::meta_get(&state.pool, key)
        .await?
        .and_then(|v| v.parse().ok()))
}
