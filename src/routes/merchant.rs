use crate::{
    activity,
    auth::{AuthUser, hash_password, user_json, verify_password},
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Multipart, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub async fn show(State(state): State<AppState>, _auth: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "merchant": merchant_json(&state).await? })))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    multipart: Multipart,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let fields = read_fields(multipart).await?;
    if let Some(name) = fields.get("name").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        db::meta_set(&state.pool, "merchant_name", name).await?;
    }
    if let Some(currency) = fields.get("currency").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        db::meta_set(&state.pool, "currency", currency).await?;
    }
    if let Some(enable_tax) = fields.get("enable_tax") {
        let flag = matches!(enable_tax.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes");
        db::meta_set(&state.pool, "enable_tax", if flag { "1" } else { "0" }).await?;
    }
    if let Some(tax_rate) = fields.get("tax_rate") {
        db::meta_set(&state.pool, "tax_rate", tax_rate.trim()).await?;
    }
    if let Some(address) = fields.get("address").or_else(|| fields.get("store_address")) {
        db::meta_set(&state.pool, "address", address.trim()).await?;
    }
    if let Some(raw) = fields.get("low_stock_alert") {
        let value = parse_alert_threshold(raw)?;
        db::meta_set(&state.pool, "low_stock_alert", &value.to_string()).await?;
    }
    if let Some(raw) = fields.get("warehouse_low_stock_alert") {
        let value = parse_alert_threshold(raw)?;
        db::meta_set(&state.pool, "warehouse_low_stock_alert", &value.to_string()).await?;
    }
    if let Some(mode) = fields.get("margin_mode") {
        let mode = mode.trim().to_lowercase();
        if !matches!(mode.as_str(), "manual" | "percent" | "amount") {
            return Err(AppError::bad("Margin mode must be manual, percent, or amount."));
        }
        db::meta_set(&state.pool, "margin_mode", &mode).await?;
    }
    if let Some(raw) = fields.get("margin_value") {
        let value = raw
            .trim()
            .parse::<f64>()
            .map_err(|_| AppError::bad("Margin value must be a number."))?;
        if value < 0.0 {
            return Err(AppError::bad("Margin value cannot be negative."));
        }
        db::meta_set(&state.pool, "margin_value", &format!("{value:.2}")).await?;
    }
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} updated business settings", auth.user.name),
        None,
    )
    .await;
    Ok(Json(json!({ "merchant": merchant_json(&state).await? })))
}

#[derive(Debug, Deserialize)]
pub struct ProfileBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub current_password: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_confirmation: Option<String>,
}

pub async fn profile_update(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<ProfileBody>,
) -> AppResult<Json<Value>> {
    if let Some(name) = body.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query("UPDATE users SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(name)
            .bind(auth.user.id)
            .execute(&state.pool)
            .await?;
    }
    if let Some(password) = body.password.as_deref().filter(|s| !s.is_empty()) {
        let current = body.current_password.as_deref().unwrap_or("");
        if !verify_password(current, &auth.user.password_digest) {
            return Err(AppError::bad("Current password is incorrect."));
        }
        if password.len() < 6 {
            return Err(AppError::bad("Password must be at least 6 characters."));
        }
        if body.password_confirmation.as_deref() != Some(password) {
            return Err(AppError::bad("Password confirmation does not match."));
        }
        let digest = hash_password(password)?;
        sqlx::query("UPDATE users SET password_digest = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&digest)
            .bind(auth.user.id)
            .execute(&state.pool)
            .await?;
    }
    let user = sqlx::query_as::<_, crate::auth::UserRow>(
        "SELECT id, cloud_user_id, name, email, role, title, status, password_digest, permissions_json FROM users WHERE id = ?",
    )
    .bind(auth.user.id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({ "user": user_json(&user) })))
}

pub async fn merchant_json(state: &AppState) -> AppResult<Value> {
    let id = db::meta_get(&state.pool, "merchant_id")
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let name = db::meta_get(&state.pool, "merchant_name")
        .await?
        .unwrap_or_else(|| "Local store".into());
    let slug = db::meta_get(&state.pool, "merchant_slug")
        .await?
        .unwrap_or_else(|| "local".into());
    let code = db::ensure_merchant_code(&state.pool).await?;
    let currency = db::meta_get(&state.pool, "currency")
        .await?
        .unwrap_or_else(|| "NGN".into());
    let enable_tax = db::meta_get(&state.pool, "enable_tax").await?.as_deref() == Some("1");
    let tax_rate = db::meta_get(&state.pool, "tax_rate")
        .await?
        .unwrap_or_else(|| "0".into());
    let address = db::meta_get(&state.pool, "address").await?;
    let low_stock_alert = meta_alert(&state, "low_stock_alert").await?;
    let warehouse_low_stock_alert = meta_alert(&state, "warehouse_low_stock_alert").await?;
    let margin_mode = db::meta_get(&state.pool, "margin_mode")
        .await?
        .filter(|v| matches!(v.as_str(), "manual" | "percent" | "amount"))
        .unwrap_or_else(|| "manual".into());
    let margin_value = db::meta_get(&state.pool, "margin_value")
        .await?
        .unwrap_or_else(|| "0".into());
    Ok(json!({
        "id": id,
        "name": name,
        "slug": slug,
        "code": code,
        "currency": currency,
        "enable_tax": enable_tax,
        "tax_rate": tax_rate,
        "margin_mode": margin_mode,
        "margin_value": margin_value,
        "online_store": false,
        "address": address,
        "store_address": address,
        "host_mode": true,
        "low_stock_alert": low_stock_alert,
        "warehouse_low_stock_alert": warehouse_low_stock_alert,
        "stock_adjust_qty_limit": 1000,
        "stock_adjust_value_limit": "1000000",
    }))
}

pub async fn meta_alert(state: &AppState, key: &str) -> AppResult<i64> {
    Ok(db::meta_get(&state.pool, key)
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(5))
}

fn parse_alert_threshold(raw: &str) -> AppResult<i64> {
    let value = raw
        .trim()
        .parse::<i64>()
        .map_err(|_| AppError::bad("Low stock alert must be a whole number."))?;
    if value < 1 {
        return Err(AppError::bad("Low stock alert must be at least 1."));
    }
    Ok(value)
}

async fn read_fields(mut multipart: Multipart) -> AppResult<std::collections::HashMap<String, String>> {
    let mut fields = std::collections::HashMap::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() || name == "logo" {
            continue;
        }
        let text = field
            .text()
            .await
            .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?;
        fields.insert(name, text);
    }
    Ok(fields)
}
