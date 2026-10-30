use crate::{
    activity,
    auth::AuthUser,
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

fn default_settings() -> Value {
    json!({
        "general": {
            "timezone": "Africa/Lagos",
            "date_format": "DD/MM/YYYY",
            "time_format": "24h",
            "language": "en",
            "session_timeout_minutes": 60,
            "receipt_footer": ""
        },
        "sales": {
            "require_customer": false,
            "max_discount_percent": 100,
            "return_reason_required": false,
            "allow_held_sales": true,
            "show_cashier_on_receipt": true,
            "enable_tax": true,
            "tax_rate": 0.0
        },
        "stock": {
            "track_lots": false,
            "track_expiry": false,
            "block_expired_sales": false,
            "low_stock_alert": 5,
            "warehouse_low_stock_alert": 5,
            "stock_adjust_qty_limit": 20,
            "stock_adjust_value_limit": 50000.0,
            "allow_negative_stock": false,
            "require_adjustment_reason": true,
            "reorder_default_days": 7
        },
        "pos": {
            "require_register_open": true,
            "require_register_close_count": false
        },
        "loyalty": {
            "enabled": false,
            "earn_rate": 1.0,
            "redeem_rate": 100.0,
            "min_redeem_points": 0
        }
    })
}

pub async fn effective_settings(state: &AppState) -> AppResult<Value> {
    let mut packs = default_settings();
    if let Some(raw) = db::meta_get(&state.pool, "settings_json").await? {
        if let Ok(Value::Object(stored)) = serde_json::from_str::<Value>(&raw) {
            if let Value::Object(ref mut root) = packs {
                for (key, value) in stored {
                    match (root.get_mut(&key), value) {
                        (Some(Value::Object(dst)), Value::Object(src)) => {
                            for (k, v) in src {
                                dst.insert(k, v);
                            }
                        }
                        (_, value) => {
                            root.insert(key, value);
                        }
                    }
                }
            }
        }
    }
    let enable_tax = db::meta_get(&state.pool, "enable_tax").await?.as_deref() == Some("1");
    let tax_rate: f64 = db::meta_get(&state.pool, "tax_rate")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    if let Some(sales) = packs.get_mut("sales").and_then(|v| v.as_object_mut()) {
        sales.insert("enable_tax".into(), json!(enable_tax));
        sales.insert("tax_rate".into(), json!(tax_rate));
    }
    if let Some(stock) = packs.get_mut("stock").and_then(|v| v.as_object_mut()) {
        overlay_meta_bool(state, stock, "track_lots").await?;
        overlay_meta_bool(state, stock, "track_expiry").await?;
        overlay_meta_bool(state, stock, "block_expired_sales").await?;
        overlay_meta_number(state, stock, "low_stock_alert", 5.0).await?;
        overlay_meta_number(state, stock, "warehouse_low_stock_alert", 5.0).await?;
        overlay_meta_number(state, stock, "stock_adjust_qty_limit", 20.0).await?;
        overlay_meta_number(state, stock, "stock_adjust_value_limit", 50000.0).await?;
    }
    Ok(packs)
}

async fn overlay_meta_bool(
    state: &AppState,
    stock: &mut serde_json::Map<String, Value>,
    key: &str,
) -> AppResult<()> {
    if let Some(raw) = db::meta_get(&state.pool, key).await? {
        let on = matches!(raw.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes");
        stock.insert(key.into(), json!(on));
    }
    Ok(())
}

async fn overlay_meta_number(
    state: &AppState,
    stock: &mut serde_json::Map<String, Value>,
    key: &str,
    fallback: f64,
) -> AppResult<()> {
    if let Some(raw) = db::meta_get(&state.pool, key).await? {
        let value = raw.trim().parse::<f64>().unwrap_or(fallback);
        stock.insert(key.into(), json!(value));
    }
    Ok(())
}

pub async fn show_settings(State(state): State<AppState>, _auth: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "settings": effective_settings(&state).await? })))
}

#[derive(Debug, Deserialize)]
pub struct SettingsBody {
    pub settings: Value,
}

pub async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SettingsBody>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let before = effective_settings(&state).await?;
    let mut current = before.clone();
    if let (Value::Object(ref mut root), Value::Object(incoming)) = (&mut current, body.settings) {
        for (pack, value) in incoming {
            match (root.get_mut(&pack), value) {
                (Some(Value::Object(dst)), Value::Object(src)) => {
                    for (k, v) in src {
                        dst.insert(k, v);
                    }
                }
                (_, value) => {
                    root.insert(pack, value);
                }
            }
        }
    }
    if let Some(sales) = current.get("sales") {
        if let Some(enable_tax) = sales.get("enable_tax").and_then(|v| v.as_bool()) {
            db::meta_set(&state.pool, "enable_tax", if enable_tax { "1" } else { "0" }).await?;
        }
        if let Some(tax_rate) = sales.get("tax_rate").and_then(|v| v.as_f64()) {
            db::meta_set(&state.pool, "tax_rate", &format!("{tax_rate:.2}")).await?;
        }
    }
    if let Some(stock) = current.get("stock") {
        sync_stock_meta_bool(&state, stock, "track_lots").await?;
        sync_stock_meta_bool(&state, stock, "track_expiry").await?;
        sync_stock_meta_bool(&state, stock, "block_expired_sales").await?;
        sync_stock_meta_number(&state, stock, "low_stock_alert", 0).await?;
        sync_stock_meta_number(&state, stock, "warehouse_low_stock_alert", 0).await?;
        sync_stock_meta_number(&state, stock, "stock_adjust_qty_limit", 0).await?;
        sync_stock_meta_number(&state, stock, "stock_adjust_value_limit", 2).await?;
    }
    if let Some(loyalty) = current.get("loyalty") {
        let enabled = loyalty.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let earn = loyalty.get("earn_rate").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let redeem = loyalty.get("redeem_rate").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let min_pts = loyalty.get("min_redeem_points").and_then(|v| v.as_i64()).unwrap_or(0);
        sqlx::query(
            "INSERT INTO loyalty_programs(id, enabled, earn_rate, redeem_rate, min_redeem_points)
             VALUES(1, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET enabled=excluded.enabled, earn_rate=excluded.earn_rate,
               redeem_rate=excluded.redeem_rate, min_redeem_points=excluded.min_redeem_points",
        )
        .bind(if enabled { 1 } else { 0 })
        .bind(earn)
        .bind(redeem)
        .bind(min_pts)
        .execute(&state.pool)
        .await?;
    }
    db::meta_set(&state.pool, "settings_json", &current.to_string()).await?;
    let changes = flatten_settings_diff(&before, &current);
    let metadata = if changes.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        Some(json!({ "changes": changes }))
    } else {
        None
    };
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} updated settings packs", auth.user.name),
        metadata,
    )
    .await;
    Ok(Json(json!({ "settings": current })))
}

async fn sync_stock_meta_bool(state: &AppState, stock: &Value, key: &str) -> AppResult<()> {
    if let Some(flag) = stock.get(key).and_then(|v| v.as_bool()) {
        db::meta_set(&state.pool, key, if flag { "1" } else { "0" }).await?;
    }
    Ok(())
}

async fn sync_stock_meta_number(
    state: &AppState,
    stock: &Value,
    key: &str,
    decimals: usize,
) -> AppResult<()> {
    let value = stock
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)));
    if let Some(value) = value {
        let text = if decimals == 0 {
            format!("{}", value.round() as i64)
        } else {
            format!("{value:.decimals$}")
        };
        db::meta_set(&state.pool, key, &text).await?;
    }
    Ok(())
}

/// Leaf-level before/after map for activity UI (`changes` key).
fn flatten_settings_diff(before: &Value, after: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let Some(before_root) = before.as_object() else {
        return Value::Object(out);
    };
    let Some(after_root) = after.as_object() else {
        return Value::Object(out);
    };
    for (pack, after_pack) in after_root {
        let before_pack = before_root.get(pack);
        match (before_pack.and_then(|v| v.as_object()), after_pack.as_object()) {
            (Some(before_fields), Some(after_fields)) => {
                for (key, new_val) in after_fields {
                    let old_val = before_fields.get(key).cloned().unwrap_or(Value::Null);
                    if &old_val != new_val {
                        out.insert(format!("{pack}.{key}"), json!([old_val, new_val]));
                    }
                }
                for (key, old_val) in before_fields {
                    if !after_fields.contains_key(key) {
                        out.insert(format!("{pack}.{key}"), json!([old_val, Value::Null]));
                    }
                }
            }
            _ => {
                let old_val = before_pack.cloned().unwrap_or(Value::Null);
                if &old_val != after_pack {
                    out.insert(pack.clone(), json!([old_val, after_pack]));
                }
            }
        }
    }
    Value::Object(out)
}

#[derive(Debug, Deserialize)]
pub struct SettingsEventBody {
    pub summary: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

pub async fn settings_events(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SettingsEventBody>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let summary = body
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Updated settings")
        .to_string();
    activity::log(
        &state,
        &auth,
        "settings.updated",
        summary,
        body.metadata,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn payment_methods_index(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> AppResult<Json<Value>> {
    let rows: Vec<(i64, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, code, name, active, sort_order, opens_drawer FROM payment_methods ORDER BY sort_order, id",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let list = if rows.is_empty() {
        json!([
            {"id":1,"code":"cash","name":"Cash","active":true,"sort_order":0,"opens_drawer":true},
            {"id":2,"code":"transfer","name":"Transfer","active":true,"sort_order":1,"opens_drawer":false},
            {"id":3,"code":"card","name":"Card","active":true,"sort_order":2,"opens_drawer":false},
            {"id":4,"code":"credit","name":"Credit","active":true,"sort_order":3,"opens_drawer":false}
        ])
    } else {
        Value::Array(
            rows.into_iter()
                .map(|(id, code, name, active, sort_order, opens_drawer)| {
                    json!({
                        "id": id,
                        "code": code,
                        "name": name,
                        "active": active != 0,
                        "sort_order": sort_order,
                        "opens_drawer": opens_drawer != 0
                    })
                })
                .collect(),
        )
    };
    Ok(Json(json!({ "payment_methods": list })))
}

#[derive(Debug, Deserialize)]
pub struct PaymentMethodBody {
    pub payment_method: Option<PaymentMethodFields>,
    pub code: Option<String>,
    pub name: Option<String>,
    pub active: Option<bool>,
    pub sort_order: Option<i64>,
    pub opens_drawer: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PaymentMethodFields {
    pub code: Option<String>,
    pub name: Option<String>,
    pub active: Option<bool>,
    pub sort_order: Option<i64>,
    pub opens_drawer: Option<bool>,
}

pub async fn payment_methods_create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PaymentMethodBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("settings.update")?;
    let fields = body.payment_method.as_ref();
    let code = fields
        .and_then(|f| f.code.clone())
        .or(body.code)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let name = fields
        .and_then(|f| f.name.clone())
        .or(body.name)
        .unwrap_or_default()
        .trim()
        .to_string();
    if code.is_empty() || name.is_empty() {
        return Err(AppError::bad("Code and name are required."));
    }
    let opens = fields
        .and_then(|f| f.opens_drawer)
        .or(body.opens_drawer)
        .unwrap_or(false);
    let sort_order = fields.and_then(|f| f.sort_order).or(body.sort_order).unwrap_or(0);
    let result = sqlx::query(
        "INSERT INTO payment_methods(code, name, active, sort_order, opens_drawer) VALUES(?, ?, 1, ?, ?)",
    )
    .bind(&code)
    .bind(&name)
    .bind(sort_order)
    .bind(if opens { 1 } else { 0 })
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::bad("Could not create payment method."))?;
    let id = result.last_insert_rowid();
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} added payment method {}", auth.user.name, name),
        Some(json!({ "id": id, "code": code, "name": name })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "payment_method": {
                "id": id,
                "code": code,
                "name": name,
                "active": true,
                "sort_order": sort_order,
                "opens_drawer": opens
            }
        })),
    ))
}

pub async fn payment_methods_update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<PaymentMethodBody>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let fields = body.payment_method.as_ref();
    let row: Option<(String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT code, name, active, sort_order, opens_drawer FROM payment_methods WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((mut code, mut name, mut active, mut sort_order, mut opens)) = row else {
        return Err(AppError::bad("Payment method not found."));
    };
    if let Some(next) = fields.and_then(|f| f.code.clone()).or(body.code) {
        code = next.trim().to_lowercase();
    }
    if let Some(next) = fields.and_then(|f| f.name.clone()).or(body.name) {
        name = next.trim().to_string();
    }
    if let Some(next) = fields.and_then(|f| f.active).or(body.active) {
        active = if next { 1 } else { 0 };
    }
    if let Some(next) = fields.and_then(|f| f.sort_order).or(body.sort_order) {
        sort_order = next;
    }
    if let Some(next) = fields.and_then(|f| f.opens_drawer).or(body.opens_drawer) {
        opens = if next { 1 } else { 0 };
    }
    sqlx::query(
        "UPDATE payment_methods SET code=?, name=?, active=?, sort_order=?, opens_drawer=?, updated_at=datetime('now') WHERE id=?",
    )
    .bind(&code)
    .bind(&name)
    .bind(active)
    .bind(sort_order)
    .bind(opens)
    .bind(id)
    .execute(&state.pool)
    .await?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} updated payment method {}", auth.user.name, name),
        Some(json!({
            "id": id,
            "code": code,
            "name": name,
            "active": active != 0,
            "opens_drawer": opens != 0
        })),
    )
    .await;
    Ok(Json(json!({
        "payment_method": {
            "id": id,
            "code": code,
            "name": name,
            "active": active != 0,
            "sort_order": sort_order,
            "opens_drawer": opens != 0
        }
    })))
}

pub async fn payment_methods_destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM payment_methods WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    sqlx::query("UPDATE payment_methods SET active = 0, updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!(
            "{} removed payment method {}",
            auth.user.name,
            name.unwrap_or_else(|| format!("#{id}"))
        ),
        Some(json!({ "id": id })),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn active_payment_codes(state: &AppState) -> AppResult<Vec<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM payment_methods WHERE active = 1 ORDER BY sort_order, id",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    if rows.is_empty() {
        Ok(vec![
            "cash".into(),
            "transfer".into(),
            "card".into(),
            "credit".into(),
        ])
    } else {
        Ok(rows)
    }
}
