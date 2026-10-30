use crate::{
    auth::AuthUser,
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
pub struct CategoryBody {
    pub name: String,
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    if !auth.allow("categories.view") && !auth.allow("products.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM categories ORDER BY name")
        .fetch_all(&state.pool)
        .await?;
    let categories: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| {
            json!({ "id": id, "name": name, "products_count": 0 })
        })
        .collect();
    Ok(Json(json!({ "categories": categories })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CategoryBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("categories.create")?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Name is required."));
    }
    let result = sqlx::query("INSERT INTO categories(name) VALUES(?)")
        .bind(name)
        .execute(&state.pool)
        .await?;
    let id = result.last_insert_rowid();
    Ok((
        StatusCode::CREATED,
        Json(json!({ "category": { "id": id, "name": name, "products_count": 0 } })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<CategoryBody>,
) -> AppResult<Json<Value>> {
    auth.require("categories.update")?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Name is required."));
    }
    let changed = sqlx::query("UPDATE categories SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Category not found.".into()));
    }
    Ok(Json(json!({ "category": { "id": id, "name": name, "products_count": 0 } })))
}

pub async fn destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require("categories.delete")?;
    let changed = sqlx::query("DELETE FROM categories WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Category not found.".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}
