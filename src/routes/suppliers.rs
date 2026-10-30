use crate::{
    activity,
    auth::AuthUser,
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
pub struct SupplierQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SupplierBody {
    pub name: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<SupplierQuery>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.view")?;
    let mut sql = String::from(
        "SELECT id, name, phone, email, company, status, balance FROM suppliers WHERE 1=1",
    );
    if query.status.as_deref() == Some("active") {
        sql.push_str(" AND status = 'active'");
    }
    let mut binds = vec![];
    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND (name LIKE ? OR IFNULL(company,'') LIKE ?)");
        let like = format!("%{q}%");
        binds.push(like.clone());
        binds.push(like);
    }
    sql.push_str(" ORDER BY name");
    let mut q = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, Option<String>, String, String)>(&sql);
    for b in &binds {
        q = q.bind(b);
    }
    let rows = q.fetch_all(&state.pool).await?;
    let suppliers: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, phone, email, company, status, balance)| {
            json!({
                "id": id,
                "name": name,
                "phone": phone,
                "email": email,
                "company": company,
                "status": status,
                "balance": balance,
            })
        })
        .collect();
    Ok(Json(json!({ "suppliers": suppliers })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.view")?;
    let row = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, Option<String>, String, String)>(
        "SELECT id, name, phone, email, company, status, balance FROM suppliers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Supplier not found.".into()))?;
    Ok(Json(json!({
        "supplier": {
            "id": row.0,
            "name": row.1,
            "phone": row.2,
            "email": row.3,
            "company": row.4,
            "status": row.5,
            "balance": row.6,
        }
    })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SupplierBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("suppliers.create")?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("Name is required."));
    }
    let result = sqlx::query(
        "INSERT INTO suppliers(name, phone, email, company, status, balance) VALUES(?, ?, ?, ?, 'active', '0')",
    )
    .bind(name)
    .bind(body.phone.as_deref())
    .bind(body.email.as_deref())
    .bind(body.company.as_deref())
    .execute(&state.pool)
    .await?;
    let id = result.last_insert_rowid();
    activity::log(
        &state,
        &auth,
        "supplier.created",
        format!("{} added supplier {name}", auth.user.name),
        Some(json!({ "name": name })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "supplier": { "id": id, "name": name, "status": "active", "balance": "0" } })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<SupplierBody>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.update")?;
    let name = body.name.trim().to_string();
    sqlx::query("UPDATE suppliers SET name = ?, phone = ?, email = ?, company = ? WHERE id = ?")
        .bind(&name)
        .bind(body.phone.as_deref())
        .bind(body.email.as_deref())
        .bind(body.company.as_deref())
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "supplier.updated",
        format!("Updated supplier {name}"),
        Some(json!({ "name": name })),
    )
    .await;
    show(State(state), auth, Path(id)).await
}

pub async fn destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require("suppliers.delete")?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM suppliers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    sqlx::query("DELETE FROM suppliers WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(name) = name {
        activity::log(
            &state,
            &auth,
            "supplier.deleted",
            format!("{} deleted supplier {name}", auth.user.name),
            Some(json!({ "name": name })),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn suspend(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.update")?;
    sqlx::query("UPDATE suppliers SET status = 'suspended' WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "supplier.suspended",
        format!("{} suspended supplier #{id}", auth.user.name),
        Some(json!({ "supplier_id": id, "status": "suspended" })),
    )
    .await;
    show(State(state), auth, Path(id)).await
}

pub async fn restore(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.update")?;
    sqlx::query("UPDATE suppliers SET status = 'active' WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "supplier.restored",
        format!("{} restored supplier #{id}", auth.user.name),
        Some(json!({ "supplier_id": id, "status": "active" })),
    )
    .await;
    show(State(state), auth, Path(id)).await
}

#[derive(Debug, Deserialize)]
pub struct PaymentBody {
    #[serde(deserialize_with = "crate::money::de_amount")]
    pub amount: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn ledger(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.view")?;
    let _ = show(State(state.clone()), auth, Path(id)).await?;
    Ok(Json(json!({
        "supplier_id": id,
        "entries": [],
        "payments": [],
        "balance": sqlx::query_scalar::<_, String>("SELECT balance FROM suppliers WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| "0".into()),
    })))
}

pub async fn create_payment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<PaymentBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("suppliers.update")?;
    let amount = crate::money::parse_money(&body.amount)?;
    if amount <= 0.0 {
        return Err(AppError::bad("Amount must be greater than 0."));
    }
    let balance: String = sqlx::query_scalar("SELECT balance FROM suppliers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Supplier not found.".into()))?;
    let current = crate::money::parse_money(&balance)?;
    let next = (current - amount).max(0.0);
    sqlx::query("UPDATE suppliers SET balance = ? WHERE id = ?")
        .bind(crate::money::format_money(next))
        .bind(id)
        .execute(&state.pool)
        .await?;
    let method = body.method.clone().unwrap_or_else(|| "cash".into());
    activity::log(
        &state,
        &auth,
        "supplier.paid",
        format!(
            "{} paid {} to supplier #{id} by {method}",
            auth.user.name,
            crate::money::format_money(amount)
        ),
        Some(json!({
            "supplier_id": id,
            "amount": crate::money::format_money(amount),
            "method": method,
        })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "payment": {
                "id": chrono::Utc::now().timestamp(),
                "supplier_id": id,
                "amount": crate::money::format_money(amount),
                "method": method,
                "note": body.note,
            }
        })),
    ))
}

pub async fn notify_payment(
    State(_state): State<AppState>,
    auth: AuthUser,
    Path((_id, _payment_id)): Path<(i64, i64)>,
) -> AppResult<Json<Value>> {
    auth.require("suppliers.update")?;
    Ok(Json(json!({ "ok": true, "message": "Notify is unavailable offline." })))
}
