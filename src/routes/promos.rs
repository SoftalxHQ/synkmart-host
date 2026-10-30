use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{Json, extract::State};
use serde_json::{Value, json};

/// Cloud-only promo desk: return empty collections offline so the UI stays quiet.
pub async fn promotions_index(State(_state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    if !auth.allow("promos.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    Ok(Json(json!({ "promotions": [], "offline": true, "message": "Promotions require SynkMart cloud." })))
}

pub async fn promo_codes_index(State(_state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    if !auth.allow("promos.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    Ok(Json(json!({ "promo_codes": [], "offline": true })))
}

pub async fn banners_index(State(_state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    if !auth.allow("promos.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    Ok(Json(json!({ "promotional_banners": [], "banners": [], "offline": true })))
}

pub async fn promotions_active(State(_state): State<AppState>, _auth: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "promotions": [] })))
}

pub async fn validate_code(State(_state): State<AppState>, _auth: AuthUser) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "valid": false, "error": "Promo codes require SynkMart cloud." })))
}
