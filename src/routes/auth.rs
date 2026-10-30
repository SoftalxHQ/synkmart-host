use crate::{
    activity,
    auth::{AuthUser, user_json, verify_password},
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SignInBody {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub merchant_code: Option<String>,
    /// POS lock-screen unlock — same password check, different activity action.
    #[serde(default)]
    pub unlock: bool,
}

pub async fn sign_in(
    State(state): State<AppState>,
    Json(body): Json<SignInBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let email = body.email.trim().to_lowercase();
    // If the POS sends a merchant code, it must match this host. Email/password alone is enough on the host PC.
    let stored_code = db::meta_get(&state.pool, "merchant_code")
        .await?
        .unwrap_or_default();
    if !stored_code.is_empty() {
        if let Some(code) = body
            .merchant_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !stored_code.eq_ignore_ascii_case(code) {
                return Err(AppError::unauthorized("Email or password is incorrect."));
            }
        }
    }

    let user = sqlx::query_as::<_, crate::auth::UserRow>(
        "SELECT id, cloud_user_id, name, email, role, title, status, password_digest, permissions_json
         FROM users WHERE lower(email) = ?",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::unauthorized("Email or password is incorrect."))?;

    if !verify_password(&body.password, &user.password_digest) {
        return Err(AppError::unauthorized("Email or password is incorrect."));
    }
    if user.status != "active" {
        let message = if user.status == "suspended" {
            "This account is suspended."
        } else {
            "This account is no longer active."
        };
        return Err(AppError::unauthorized(message));
    }

    let token = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO sessions(token, user_id) VALUES(?, ?)")
        .bind(&token)
        .bind(user.id)
        .execute(&state.pool)
        .await?;

    let (action, summary) = if body.unlock {
        (
            "auth.unlocked",
            format!("{} unlocked the register", user.name),
        )
    } else {
        ("auth.signed_in", format!("{} signed in", user.name))
    };
    activity::log_user(
        &state,
        user.id,
        &user.name,
        action,
        summary,
        Some(json!({
            "email": user.email,
            "role": user.role,
            "unlock": body.unlock,
        })),
    )
    .await;

    let merchant = merchant_json(&state).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "token": token,
            "user": user_json(&user),
            "merchant": merchant,
        })),
    ))
}

pub async fn sign_out(State(state): State<AppState>, headers: HeaderMap) -> StatusCode {
    // Soft logout: never 401 on sign-out — stale tokens after a host re-seed are common.
    if let Some(token) = bearer_token(&headers) {
        if let Ok(Some((user_id, name))) = sqlx::query_as::<_, (i64, String)>(
            "SELECT u.id, u.name FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token = ?",
        )
        .bind(token)
        .fetch_optional(&state.pool)
        .await
        {
            activity::log_user(
                &state,
                user_id,
                &name,
                "auth.signed_out",
                format!("{name} signed out"),
                None,
            )
            .await;
        }
        let _ = sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&state.pool)
            .await;
    }
    StatusCode::NO_CONTENT
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .unwrap_or(header)
        .trim();
    (!token.is_empty()).then_some(token)
}

pub async fn me(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    let merchant = merchant_json(&state).await?;
    Ok(Json(json!({
        "user": user_json(&auth.user),
        "merchant": merchant,
    })))
}

pub async fn lock(State(state): State<AppState>, auth: AuthUser) -> AppResult<StatusCode> {
    activity::log_user(
        &state,
        auth.user.id,
        &auth.user.name,
        "auth.locked",
        format!("{} locked the register", auth.user.name),
        Some(json!({ "email": auth.user.email, "role": auth.user.role })),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn merchant_json(state: &AppState) -> AppResult<Value> {
    super::merchant::merchant_json(state).await
}
