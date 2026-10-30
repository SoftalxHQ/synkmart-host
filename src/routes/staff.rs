use crate::{
    activity,
    auth::{AuthUser, hash_password, user_json},
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

#[derive(Debug, Deserialize)]
pub struct StaffBody {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("staff.view")?;
    let rows = sqlx::query_as::<_, crate::auth::UserRow>(
        "SELECT id, cloud_user_id, name, email, role, title, status, password_digest, permissions_json
         FROM users ORDER BY name",
    )
    .fetch_all(&state.pool)
    .await?;
    let staff: Vec<Value> = rows.iter().map(staff_json).collect();
    Ok(Json(json!({ "staff": staff })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<StaffBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("staff.create")?;
    let name = body.name.trim();
    let email = body.email.trim().to_lowercase();
    let password = body.password.as_deref().unwrap_or("");
    if name.is_empty() || email.is_empty() || password.len() < 6 {
        return Err(AppError::bad("Name, email, and a password of at least 6 characters are required."));
    }
    let role = normalize_role(body.role.as_deref());
    let permissions = body.permissions.clone().unwrap_or_default();
    let digest = hash_password(password)?;
    let result = sqlx::query(
        "INSERT INTO users(name, email, role, title, status, password_digest, permissions_json)
         VALUES(?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(name)
    .bind(&email)
    .bind(role)
    .bind(body.title.as_deref())
    .bind(&digest)
    .bind(serde_json::to_string(&permissions).unwrap_or_else(|_| "[]".into()))
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::bad("That email is already in use.")
        } else {
            AppError::Db(e)
        }
    })?;
    let id = result.last_insert_rowid();
    let row = load(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "staff.created",
        format!(
            "{} added staff {} as {} ({email})",
            auth.user.name,
            row.name,
            row.title.as_deref().unwrap_or(&row.role)
        ),
        Some(json!({ "name": row.name, "email": row.email, "role": row.role })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({ "staff": staff_json(&row), "user": user_json(&row) }))))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<StaffBody>,
) -> AppResult<Json<Value>> {
    auth.require("staff.update")?;
    let row = load(&state, id).await?;
    let name = body.name.trim();
    let email = body.email.trim().to_lowercase();
    let role = normalize_role(body.role.as_deref().or(Some(&row.role)));
    let permissions = body
        .permissions
        .clone()
        .unwrap_or_else(|| serde_json::from_str(&row.permissions_json).unwrap_or_default());
    if let Some(password) = body.password.as_deref().filter(|p| !p.is_empty()) {
        if password.len() < 6 {
            return Err(AppError::bad("Password must be at least 6 characters."));
        }
        let digest = hash_password(password)?;
        sqlx::query(
            "UPDATE users SET name = ?, email = ?, role = ?, title = ?, password_digest = ?, permissions_json = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(name)
        .bind(&email)
        .bind(role)
        .bind(body.title.as_deref())
        .bind(&digest)
        .bind(serde_json::to_string(&permissions).unwrap_or_else(|_| "[]".into()))
        .bind(id)
        .execute(&state.pool)
        .await?;
    } else {
        sqlx::query(
            "UPDATE users SET name = ?, email = ?, role = ?, title = ?, permissions_json = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(name)
        .bind(&email)
        .bind(role)
        .bind(body.title.as_deref())
        .bind(serde_json::to_string(&permissions).unwrap_or_else(|_| "[]".into()))
        .bind(id)
        .execute(&state.pool)
        .await?;
    }
    let row = load(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "staff.updated",
        format!("Updated {}", row.name),
        Some(json!({ "name": row.name, "email": row.email, "role": row.role })),
    )
    .await;
    Ok(Json(json!({ "staff": staff_json(&row), "user": user_json(&row) })))
}

pub async fn suspend(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("staff.update")?;
    if id == auth.user.id {
        return Err(AppError::bad("You cannot suspend your own account."));
    }
    sqlx::query("UPDATE users SET status = 'suspended', updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    let row = load(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "staff.suspended",
        format!(
            "{} suspended {} ({})",
            auth.user.name,
            row.name,
            row.title.as_deref().unwrap_or(&row.role)
        ),
        Some(json!({ "name": row.name, "status": "suspended" })),
    )
    .await;
    Ok(Json(json!({ "staff": staff_json(&row) })))
}

pub async fn restore(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("staff.update")?;
    sqlx::query("UPDATE users SET status = 'active', updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    let row = load(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "staff.restored",
        format!(
            "{} restored {} ({})",
            auth.user.name,
            row.name,
            row.title.as_deref().unwrap_or(&row.role)
        ),
        Some(json!({ "name": row.name, "status": "active" })),
    )
    .await;
    Ok(Json(json!({ "staff": staff_json(&row) })))
}

pub async fn destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require("staff.delete")?;
    if id == auth.user.id {
        return Err(AppError::bad("You cannot remove your own account."));
    }
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    sqlx::query("UPDATE users SET status = 'removed', updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(name) = name {
        activity::log(
            &state,
            &auth,
            "staff.deleted",
            format!("{} removed staff {name}", auth.user.name),
            Some(json!({ "name": name })),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn load(state: &AppState, id: i64) -> AppResult<crate::auth::UserRow> {
    sqlx::query_as::<_, crate::auth::UserRow>(
        "SELECT id, cloud_user_id, name, email, role, title, status, password_digest, permissions_json FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Staff member not found.".into()))
}

fn normalize_role(role: Option<&str>) -> &'static str {
    match role.map(str::trim).unwrap_or("cashier") {
        "admin" => "admin",
        "manager" => "manager",
        _ => "cashier",
    }
}

fn staff_json(user: &crate::auth::UserRow) -> Value {
    let mut value = user_json(user);
    value["status"] = json!(user.status);
    value
}
