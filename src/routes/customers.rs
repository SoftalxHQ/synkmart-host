use crate::{
    activity,
    auth::AuthUser,
    error::{AppError, AppResult},
    money::{de_opt_amount, format_money, parse_money},
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
pub struct CustomerQuery {
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CustomerBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default, deserialize_with = "de_opt_amount")]
    pub credit_limit: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepayBody {
    #[serde(deserialize_with = "crate::money::de_amount")]
    pub amount: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub method: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<CustomerQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("customers.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let mut sql = String::from(
        "SELECT id, name, phone, email, credit_limit, credit_balance, notes FROM customers WHERE 1=1",
    );
    let mut binds = vec![];
    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND (name LIKE ? OR IFNULL(phone,'') LIKE ? OR IFNULL(email,'') LIKE ?)");
        let like = format!("%{q}%");
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like);
    }
    sql.push_str(" ORDER BY name LIMIT 200");
    let mut q = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, String, String, Option<String>)>(&sql);
    for b in &binds {
        q = q.bind(b);
    }
    let rows = q.fetch_all(&state.pool).await?;
    let customers: Vec<Value> = rows.into_iter().map(customer_json).collect();
    Ok(Json(json!({ "customers": customers })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    if !auth.allow("customers.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let row = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, String, String, Option<String>)>(
        "SELECT id, name, phone, email, credit_limit, credit_balance, notes FROM customers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Customer not found.".into()))?;
    Ok(Json(json!({ "customer": customer_json(row) })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CustomerBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("customers.create")?;
    let name = body
        .display_name
        .as_deref()
        .or(body.name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad("Name is required."))?;
    let limit = body.credit_limit.as_deref().unwrap_or("0");
    let result = sqlx::query(
        "INSERT INTO customers(name, phone, email, credit_limit, credit_balance, notes) VALUES(?, ?, ?, ?, '0', ?)",
    )
    .bind(name)
    .bind(body.phone.as_deref())
    .bind(body.email.as_deref())
    .bind(limit)
    .bind(body.notes.as_deref())
    .execute(&state.pool)
    .await?;
    let id = result.last_insert_rowid();
    let row = (
        id,
        name.to_string(),
        body.phone.clone(),
        body.email.clone(),
        limit.to_string(),
        "0".into(),
        body.notes.clone(),
    );
    activity::log(
        &state,
        &auth,
        "customer.created",
        format!("{} added customer {name}", auth.user.name),
        Some(json!({ "name": name, "phone": body.phone })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({ "customer": customer_json(row) }))))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<CustomerBody>,
) -> AppResult<Json<Value>> {
    auth.require("customers.update")?;
    let name = body
        .display_name
        .as_deref()
        .or(body.name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad("Name is required."))?;
    let limit = body.credit_limit.as_deref().unwrap_or("0");
    let changed = sqlx::query(
        "UPDATE customers SET name = ?, phone = ?, email = ?, credit_limit = ?, notes = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(name)
    .bind(body.phone.as_deref())
    .bind(body.email.as_deref())
    .bind(limit)
    .bind(body.notes.as_deref())
    .bind(id)
    .execute(&state.pool)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Customer not found.".into()));
    }
    activity::log(
        &state,
        &auth,
        "customer.updated",
        format!("Updated customer {name}"),
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
    auth.require("customers.delete")?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM customers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    sqlx::query("DELETE FROM customers WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(name) = name {
        activity::log(
            &state,
            &auth,
            "customer.deleted",
            format!("{} deleted customer {name}", auth.user.name),
            Some(json!({ "name": name })),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn repay(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<RepayBody>,
) -> AppResult<Json<Value>> {
    auth.require("customers.update")?;
    let amount = parse_money(&body.amount)?;
    if amount <= 0.0 {
        return Err(AppError::bad("Amount must be greater than 0."));
    }
    let balance: String = sqlx::query_scalar("SELECT credit_balance FROM customers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Customer not found.".into()))?;
    let current = parse_money(&balance)?;
    let next = (current - amount).max(0.0);
    sqlx::query("UPDATE customers SET credit_balance = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(format_money(next))
        .bind(id)
        .execute(&state.pool)
        .await?;
    activity::log(
        &state,
        &auth,
        "customer.repaid",
        format!(
            "{} recorded a repayment of {} for customer #{id}",
            auth.user.name,
            format_money(amount)
        ),
        Some(json!({ "amount": format_money(amount), "customer_id": id })),
    )
    .await;
    show(State(state), auth, Path(id)).await
}

pub async fn transactions(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    if !auth.allow("customers.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let _ = sqlx::query_scalar::<_, i64>("SELECT id FROM customers WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Customer not found.".into()))?;
    // Sales linked by customer name are exposed via sales history; dedicated ledger is empty offline until sync.
    Ok(Json(json!({ "transactions": [], "customer_id": id })))
}

pub async fn transaction_show(
    State(_state): State<AppState>,
    auth: AuthUser,
    Path((_id, _txn_id)): Path<(i64, i64)>,
) -> AppResult<Json<Value>> {
    if !auth.allow("customers.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    Err(AppError::NotFound("Transaction not found.".into()))
}

fn customer_json(row: (i64, String, Option<String>, Option<String>, String, String, Option<String>)) -> Value {
    let (id, name, phone, email, credit_limit, credit_balance, notes) = row;
    json!({
        "id": id,
        "name": name,
        "display_name": name,
        "full_name": name,
        "phone": phone,
        "email": email,
        "credit_limit": credit_limit,
        "credit_balance": credit_balance,
        "current_credit": credit_balance,
        "notes": notes,
    })
}
