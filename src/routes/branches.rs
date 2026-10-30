use crate::{
    activity,
    auth::AuthUser,
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct BranchBody {
    pub name: String,
    #[serde(default)]
    pub city: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LocationBody {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LocationQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub active_branch: Option<String>,
}

pub async fn ensure_defaults(state: &AppState) -> AppResult<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM branches")
        .fetch_one(&state.pool)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let name = db::meta_get(&state.pool, "branch_name")
        .await?
        .unwrap_or_else(|| "Branch 1".into());
    sqlx::query("INSERT INTO branches(id, name, slug, city, status) VALUES(1, ?, 'main', '', 'active')")
        .bind(&name)
        .execute(&state.pool)
        .await?;
    sqlx::query(
        "INSERT INTO stock_locations(id, branch_id, name, kind, slug, status) VALUES
         (1, 1, 'Warehouse', 'warehouse', 'warehouse', 'active'),
         (2, 1, 'Shop floor', 'store', 'shop-floor', 'active')",
    )
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    if !auth.allow("settings.view") && !auth.allow("products.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let _ = ensure_defaults(&state).await;
    let rows: Vec<(i64, String, String, String, String)> =
        sqlx::query_as("SELECT id, name, slug, city, status FROM branches ORDER BY id")
            .fetch_all(&state.pool)
            .await?;
    let branches: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, slug, city, status)| {
            json!({ "id": id, "name": name, "slug": slug, "city": city, "status": status })
        })
        .collect();
    Ok(Json(json!({ "branches": branches })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    if !auth.allow("settings.view") && !auth.allow("products.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let row: Option<(i64, String, String, String, String)> =
        sqlx::query_as("SELECT id, name, slug, city, status FROM branches WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((id, name, slug, city, status)) = row else {
        return Err(AppError::NotFound("Branch not found.".into()));
    };
    let locations = location_rows(&state, id).await?;
    Ok(Json(json!({
        "branch": { "id": id, "name": name, "slug": slug, "city": city, "status": status },
        "stock_locations": locations,
    })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<BranchBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("settings.update")?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Name is required."));
    }
    let city = body.city.as_deref().unwrap_or("").trim();
    let slug = slugify(name);
    let result = sqlx::query("INSERT INTO branches(name, slug, city, status) VALUES(?, ?, ?, 'active')")
        .bind(name)
        .bind(&slug)
        .bind(city)
        .execute(&state.pool)
        .await?;
    let id = result.last_insert_rowid();
    sqlx::query(
        "INSERT INTO stock_locations(branch_id, name, kind, slug, status) VALUES
         (?, 'Warehouse', 'warehouse', 'warehouse', 'active'),
         (?, 'Shop floor', 'store', 'shop-floor', 'active')",
    )
    .bind(id)
    .bind(id)
    .execute(&state.pool)
    .await?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} added location {name}", auth.user.name),
        Some(json!({ "branch": name, "city": city, "slug": slug })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "branch": { "id": id, "name": name, "slug": slug, "city": city, "status": "active" } })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<BranchBody>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let name = body.name.trim();
    let city = body.city.as_deref().unwrap_or("").trim();
    let changed = sqlx::query("UPDATE branches SET name = ?, city = ? WHERE id = ?")
        .bind(name)
        .bind(city)
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Branch not found.".into()));
    }
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("Updated location {name}"),
        Some(json!({ "branch": name, "city": city })),
    )
    .await;
    Ok(Json(json!({ "branch": { "id": id, "name": name, "city": city, "status": "active" } })))
}

pub async fn destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require("settings.update")?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM branches WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    sqlx::query("DELETE FROM stock_locations WHERE branch_id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    let changed = sqlx::query("DELETE FROM branches WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Branch not found.".into()));
    }
    if let Some(name) = name {
        activity::log(
            &state,
            &auth,
            "settings.updated",
            format!("{} deleted location {name}", auth.user.name),
            Some(json!({ "branch": name })),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn disable(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    sqlx::query("UPDATE branches SET status = 'disabled' WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} disabled location #{id}", auth.user.name),
        Some(json!({ "branch_id": id, "status": "disabled" })),
    )
    .await;
    Ok(Json(json!({ "branch": { "id": id, "status": "disabled" } })))
}

pub async fn restore(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    sqlx::query("UPDATE branches SET status = 'active' WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} restored location #{id}", auth.user.name),
        Some(json!({ "branch_id": id, "status": "active" })),
    )
    .await;
    Ok(Json(json!({ "branch": { "id": id, "status": "active" } })))
}

pub async fn merchant_locations(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<LocationQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("sales") && !auth.allow("settings.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let _ = ensure_defaults(&state).await;
    let mut sql = String::from(
        "SELECT l.id, l.branch_id, l.name, l.kind, l.slug, l.status, b.name
         FROM stock_locations l
         JOIN branches b ON b.id = l.branch_id
         WHERE 1=1",
    );
    if query.status.as_deref() == Some("active") {
        sql.push_str(" AND l.status = 'active' AND b.status = 'active'");
    }
    if query.active_branch.as_deref() == Some("1") {
        sql.push_str(" AND b.status = 'active'");
    }
    sql.push_str(" ORDER BY b.id, l.kind, l.name");
    let rows: Vec<(i64, i64, String, String, String, String, String)> =
        sqlx::query_as(&sql).fetch_all(&state.pool).await?;
    let mut stock_locations = Vec::with_capacity(rows.len());
    for (id, branch_id, name, kind, slug, status, branch_name) in rows {
        let on_hand = location_on_hand(&state, id).await?;
        stock_locations.push(json!({
            "id": id,
            "branch_id": branch_id,
            "name": name,
            "kind": kind,
            "slug": slug,
            "status": status,
            "branch_name": branch_name,
            "label": format!("{branch_name} · {name}"),
            "on_hand": on_hand,
        }));
    }
    Ok(Json(json!({ "stock_locations": stock_locations })))
}

pub async fn locations_index(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(branch_id): Path<i64>,
) -> AppResult<Json<Value>> {
    if !auth.allow("settings.view") && !auth.allow("products.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    Ok(Json(json!({ "stock_locations": location_rows(&state, branch_id).await? })))
}

pub async fn locations_create(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(branch_id): Path<i64>,
    Json(body): Json<LocationBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("settings.update")?;
    let name = body.name.trim();
    let kind = match body.kind.as_deref().unwrap_or("store") {
        "warehouse" => "warehouse",
        _ => "store",
    };
    let slug = slugify(name);
    let result = sqlx::query(
        "INSERT INTO stock_locations(branch_id, name, kind, slug, status) VALUES(?, ?, ?, ?, 'active')",
    )
    .bind(branch_id)
    .bind(name)
    .bind(kind)
    .bind(&slug)
    .execute(&state.pool)
    .await?;
    let id = result.last_insert_rowid();
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} added {kind} {name}", auth.user.name),
        Some(json!({ "branch_id": branch_id, "name": name, "kind": kind })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "stock_location": { "id": id, "branch_id": branch_id, "name": name, "kind": kind, "slug": slug, "status": "active" } })),
    ))
}

pub async fn locations_update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((branch_id, id)): Path<(i64, i64)>,
    Json(body): Json<LocationBody>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    let name = body.name.trim();
    // Never default missing kind to "store" — settings rename sends name only and would
    // flip warehouses to store, making warehouse imports show as store stock.
    let kind = body.kind.as_deref().map(str::trim).and_then(|value| match value {
        "warehouse" => Some("warehouse"),
        "store" => Some("store"),
        _ => None,
    });
    if let Some(kind) = kind {
        sqlx::query("UPDATE stock_locations SET name = ?, kind = ? WHERE id = ? AND branch_id = ?")
            .bind(name)
            .bind(kind)
            .bind(id)
            .bind(branch_id)
            .execute(&state.pool)
            .await?;
    } else {
        sqlx::query("UPDATE stock_locations SET name = ? WHERE id = ? AND branch_id = ?")
            .bind(name)
            .bind(id)
            .bind(branch_id)
            .execute(&state.pool)
            .await?;
    }
    let (saved_name, saved_kind, status): (String, String, String) = sqlx::query_as(
        "SELECT name, kind, status FROM stock_locations WHERE id = ? AND branch_id = ?",
    )
    .bind(id)
    .bind(branch_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Location not found.".into()))?;
    activity::log(
        &state,
        &auth,
        "settings.updated",
        format!("{} updated {saved_kind} {saved_name}", auth.user.name),
        Some(json!({ "branch_id": branch_id, "name": saved_name, "kind": saved_kind })),
    )
    .await;
    Ok(Json(json!({
        "stock_location": {
            "id": id,
            "branch_id": branch_id,
            "name": saved_name,
            "kind": saved_kind,
            "status": status,
        }
    })))
}

pub async fn locations_disable(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((branch_id, id)): Path<(i64, i64)>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    sqlx::query("UPDATE stock_locations SET status = 'disabled' WHERE id = ? AND branch_id = ?")
        .bind(id)
        .bind(branch_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "stock_location": { "id": id, "status": "disabled" } })))
}

pub async fn locations_restore(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((branch_id, id)): Path<(i64, i64)>,
) -> AppResult<Json<Value>> {
    auth.require("settings.update")?;
    sqlx::query("UPDATE stock_locations SET status = 'active' WHERE id = ? AND branch_id = ?")
        .bind(id)
        .bind(branch_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "stock_location": { "id": id, "status": "active" } })))
}

pub async fn locations_destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((branch_id, id)): Path<(i64, i64)>,
) -> AppResult<StatusCode> {
    auth.require("settings.update")?;
    sqlx::query("DELETE FROM stock_locations WHERE id = ? AND branch_id = ?")
        .bind(id)
        .bind(branch_id)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn location_rows(state: &AppState, branch_id: i64) -> AppResult<Vec<Value>> {
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, kind, slug, status FROM stock_locations WHERE branch_id = ? ORDER BY kind, name",
    )
    .bind(branch_id)
    .fetch_all(&state.pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, kind, slug, status) in rows {
        let on_hand = location_on_hand(state, id).await?;
        out.push(json!({
            "id": id,
            "branch_id": branch_id,
            "name": name,
            "kind": kind,
            "slug": slug,
            "status": status,
            "on_hand": on_hand,
        }));
    }
    Ok(out)
}

async fn location_on_hand(state: &AppState, location_id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(on_hand), 0) FROM inventory_balances WHERE stock_location_id = ?",
    )
    .bind(location_id)
    .fetch_one(&state.pool)
    .await?)
}

fn slugify(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() { "location".into() } else { trimmed.into() }
}
