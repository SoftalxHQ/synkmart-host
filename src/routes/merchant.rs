use crate::{
    activity,
    auth::{AuthUser, hash_password, user_json, verify_password},
    config::default_data_dir,
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Multipart, State},
    http::{HeaderMap, HeaderValue, header},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

pub async fn show(State(state): State<AppState>, _auth: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "merchant": merchant_json(&state).await? })))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    multipart: Multipart,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let form = read_form(multipart).await?;
    let fields = &form.fields;
    let mut changes = serde_json::Map::new();

    async fn track_meta(
        state: &AppState,
        changes: &mut serde_json::Map<String, Value>,
        key: &str,
        next: &str,
    ) -> AppResult<()> {
        let before = db::meta_get(&state.pool, key).await?.unwrap_or_default();
        if before != next {
            changes.insert(key.to_string(), json!([before, next]));
        }
        db::meta_set(&state.pool, key, next).await?;
        Ok(())
    }

    if let Some(name) = fields.get("name").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        track_meta(&state, &mut changes, "merchant_name", name).await?;
    }
    if let Some(currency) = fields.get("currency").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        track_meta(&state, &mut changes, "currency", currency).await?;
    }
    if let Some(description) = fields.get("description") {
        track_meta(&state, &mut changes, "description", description.trim()).await?;
    }
    if let Some(country) = fields.get("country").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        track_meta(&state, &mut changes, "country", country).await?;
    }
    if let Some(region) = fields.get("state") {
        track_meta(&state, &mut changes, "state", region.trim()).await?;
    }
    if let Some(lga) = fields.get("lga") {
        track_meta(&state, &mut changes, "lga", lga.trim()).await?;
    }
    if let Some(city) = fields.get("city") {
        track_meta(&state, &mut changes, "city", city.trim()).await?;
    }
    if let Some(enable_tax) = fields.get("enable_tax") {
        let flag = matches!(enable_tax.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes");
        let next = if flag { "1" } else { "0" };
        track_meta(&state, &mut changes, "enable_tax", next).await?;
    }
    if let Some(tax_rate) = fields.get("tax_rate") {
        track_meta(&state, &mut changes, "tax_rate", tax_rate.trim()).await?;
    }
    if let Some(address) = fields.get("address").or_else(|| fields.get("store_address")) {
        track_meta(&state, &mut changes, "address", address.trim()).await?;
    }
    if let Some(raw) = fields.get("low_stock_alert") {
        let value = parse_alert_threshold(raw)?;
        track_meta(&state, &mut changes, "low_stock_alert", &value.to_string()).await?;
    }
    if let Some(raw) = fields.get("warehouse_low_stock_alert") {
        let value = parse_alert_threshold(raw)?;
        track_meta(
            &state,
            &mut changes,
            "warehouse_low_stock_alert",
            &value.to_string(),
        )
        .await?;
    }
    if let Some(mode) = fields.get("margin_mode") {
        let mode = mode.trim().to_lowercase();
        if !matches!(mode.as_str(), "manual" | "percent" | "amount") {
            return Err(AppError::bad("Margin mode must be manual, percent, or amount."));
        }
        track_meta(&state, &mut changes, "margin_mode", &mode).await?;
    }
    if let Some(raw) = fields.get("margin_value") {
        let value = raw
            .trim()
            .parse::<f64>()
            .map_err(|_| AppError::bad("Margin value must be a number."))?;
        if value < 0.0 {
            return Err(AppError::bad("Margin value cannot be negative."));
        }
        track_meta(&state, &mut changes, "margin_value", &format!("{value:.2}")).await?;
    }
    if let Some(online_store) = fields.get("online_store") {
        let flag = matches!(
            online_store.trim().to_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        );
        track_meta(&state, &mut changes, "online_store", if flag { "1" } else { "0" }).await?;
    }
    if form.remove_logo {
        clear_logo(&state).await?;
        changes.insert("logo".into(), json!(["attached", "removed"]));
    } else if let Some((content_type, bytes)) = form.logo {
        save_logo(&state, &content_type, &bytes).await?;
        changes.insert("logo".into(), json!(["none", "attached"]));
    }
    let metadata = if changes.is_empty() {
        None
    } else {
        Some(json!({ "changes": Value::Object(changes) }))
    };
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} updated business settings", auth.user.name),
        metadata,
    )
    .await;
    Ok(Json(json!({ "merchant": merchant_json(&state).await? })))
}

pub async fn show_logo(State(state): State<AppState>) -> AppResult<impl IntoResponse> {
    let path = logo_path();
    if !path.exists() {
        return Err(AppError::NotFound("Logo not found.".into()));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("Logo not found.".into()))?;
    let content_type = db::meta_get(&state.pool, "logo_content_type")
        .await?
        .unwrap_or_else(|| "image/png".into());
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("image/png")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    Ok((headers, Bytes::from(bytes)))
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
    let description = db::meta_get(&state.pool, "description").await?;
    let country = db::meta_get(&state.pool, "country").await?;
    let region = db::meta_get(&state.pool, "state").await?;
    let lga = db::meta_get(&state.pool, "lga").await?;
    let city = db::meta_get(&state.pool, "city").await?;
    let low_stock_alert = meta_alert(&state, "low_stock_alert").await?;
    let warehouse_low_stock_alert = meta_alert(&state, "warehouse_low_stock_alert").await?;
    let stock_adjust_qty_limit = db::meta_get(&state.pool, "stock_adjust_qty_limit")
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(20);
    let stock_adjust_value_limit = db::meta_get(&state.pool, "stock_adjust_value_limit")
        .await?
        .unwrap_or_else(|| "50000".into());
    let track_lots = db::meta_get(&state.pool, "track_lots").await?.as_deref() == Some("1");
    let track_expiry = db::meta_get(&state.pool, "track_expiry").await?.as_deref() == Some("1");
    let block_expired_sales =
        db::meta_get(&state.pool, "block_expired_sales").await?.as_deref() == Some("1");
    let margin_mode = db::meta_get(&state.pool, "margin_mode")
        .await?
        .filter(|v| matches!(v.as_str(), "manual" | "percent" | "amount"))
        .unwrap_or_else(|| "manual".into());
    let margin_value = db::meta_get(&state.pool, "margin_value")
        .await?
        .unwrap_or_else(|| "0".into());
    let online_store = db::meta_get(&state.pool, "online_store").await?.as_deref() == Some("1");
    let logo_version = db::meta_get(&state.pool, "logo_version")
        .await?
        .unwrap_or_else(|| "0".into());
    let logo_url = if logo_path().exists() {
        Some(format!("/api/v1/merchant/logo?v={logo_version}"))
    } else {
        None
    };
    let settings = super::settings::effective_settings(state).await.unwrap_or_else(|_| json!({}));
    Ok(json!({
        "id": id,
        "name": name,
        "slug": slug,
        "code": code,
        "currency": currency,
        "description": description,
        "country": country,
        "state": region,
        "lga": lga,
        "city": city,
        "enable_tax": enable_tax,
        "tax_rate": tax_rate,
        "margin_mode": margin_mode,
        "margin_value": margin_value,
        "online_store": online_store,
        "address": address,
        "store_address": address,
        "logo_url": logo_url,
        "host_mode": true,
        "low_stock_alert": low_stock_alert,
        "warehouse_low_stock_alert": warehouse_low_stock_alert,
        "stock_adjust_qty_limit": stock_adjust_qty_limit,
        "stock_adjust_value_limit": stock_adjust_value_limit,
        "track_lots": track_lots,
        "track_expiry": track_expiry,
        "block_expired_sales": block_expired_sales,
        "settings": settings,
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

struct MerchantForm {
    fields: std::collections::HashMap<String, String>,
    logo: Option<(String, Vec<u8>)>,
    remove_logo: bool,
}

async fn read_form(mut multipart: Multipart) -> AppResult<MerchantForm> {
    let mut fields = std::collections::HashMap::new();
    let mut logo = None;
    let mut remove_logo = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if name == "logo" {
            let content_type = field
                .content_type()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "image/png".into());
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad(format!("Invalid logo upload: {e}")))?;
            if !bytes.is_empty() {
                if bytes.len() > 5_000_000 {
                    return Err(AppError::bad("Logo must be 5MB or smaller."));
                }
                if !content_type.starts_with("image/") {
                    return Err(AppError::bad("Logo must be an image file."));
                }
                logo = Some((content_type, bytes.to_vec()));
            }
            continue;
        }
        let text = field
            .text()
            .await
            .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?;
        if name == "remove_logo"
            && matches!(text.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes")
        {
            remove_logo = true;
        }
        fields.insert(name, text);
    }
    Ok(MerchantForm {
        fields,
        logo,
        remove_logo,
    })
}

fn logo_path() -> PathBuf {
    default_data_dir().join("merchant").join("logo.bin")
}

async fn save_logo(state: &AppState, content_type: &str, bytes: &[u8]) -> AppResult<()> {
    let dir = logo_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_dir().join("merchant"));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::bad(format!("Could not store logo: {e}")))?;
    tokio::fs::write(logo_path(), bytes)
        .await
        .map_err(|e| AppError::bad(format!("Could not store logo: {e}")))?;
    db::meta_set(&state.pool, "logo_content_type", content_type).await?;
    let version = chrono::Utc::now().timestamp().to_string();
    db::meta_set(&state.pool, "logo_version", &version).await?;
    Ok(())
}

async fn clear_logo(state: &AppState) -> AppResult<()> {
    let path = logo_path();
    if path.exists() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    db::meta_set(&state.pool, "logo_content_type", "").await?;
    let version = chrono::Utc::now().timestamp().to_string();
    db::meta_set(&state.pool, "logo_version", &version).await?;
    Ok(())
}
