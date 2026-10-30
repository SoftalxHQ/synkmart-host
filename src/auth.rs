use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserRow {
    pub id: i64,
    pub cloud_user_id: Option<i64>,
    pub name: String,
    pub email: String,
    pub role: String,
    pub title: Option<String>,
    pub status: String,
    pub password_digest: String,
    pub permissions_json: String,
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user: UserRow,
    pub token: String,
}

impl AuthUser {
    pub fn permissions(&self) -> Vec<String> {
        serde_json::from_str::<Vec<String>>(&self.user.permissions_json).unwrap_or_default()
    }

    pub fn is_admin(&self) -> bool {
        self.user.role == "admin"
    }

    pub fn allow(&self, permission: &str) -> bool {
        if self.is_admin() {
            return true;
        }
        let list = self.permissions();
        list.iter().any(|p| p == permission || p == desk_for(permission))
    }

    pub fn require(&self, permission: &str) -> AppResult<()> {
        if self.allow(permission) {
            Ok(())
        } else {
            Err(AppError::forbidden("You do not have access to that."))
        }
    }
}

fn desk_for(permission: &str) -> &str {
    permission.split('.').next().unwrap_or(permission)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::unauthorized("Sign in to continue."))?;

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .unwrap_or(header)
            .trim();
        if token.is_empty() {
            return Err(AppError::unauthorized("Sign in to continue."));
        }

        let user = sqlx::query_as::<_, UserRow>(
            "SELECT u.id, u.cloud_user_id, u.name, u.email, u.role, u.title, u.status,
                    u.password_digest, u.permissions_json
             FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.token = ?",
        )
        .bind(token)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::unauthorized("Sign in to continue."))?;

        if user.status != "active" {
            return Err(AppError::unauthorized("This account is no longer active."));
        }

        Ok(AuthUser {
            user,
            token: token.to_string(),
        })
    }
}

pub fn verify_password(password: &str, digest: &str) -> bool {
    bcrypt::verify(password, digest).unwrap_or(false)
}

pub fn hash_password(password: &str) -> AppResult<String> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| AppError::Other(e.into()))
}

pub fn user_json(user: &UserRow) -> Value {
    let permissions: Value = serde_json::from_str(&user.permissions_json).unwrap_or(Value::Array(vec![]));
    serde_json::json!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
        "role": user.role,
        "title": user.title,
        "permissions": permissions,
    })
}
