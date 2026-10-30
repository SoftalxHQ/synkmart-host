use crate::{
    activity,
    auth::AuthUser,
    db,
    error::{AppError, AppResult},
    money::format_money,
    outbox,
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
pub struct AdjustmentBody {
    pub reason: String,
    #[serde(default)]
    pub stock_location_id: Option<i64>,
    #[serde(default)]
    pub items: Option<Vec<AdjustmentItemIn>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AdjustmentItemIn {
    pub product_id: i64,
    pub action: String,
    pub quantity: i64,
}

#[derive(Debug, Deserialize)]
pub struct RejectBody {
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MovementQuery {
    #[serde(default)]
    pub product_id: Option<i64>,
    #[serde(default)]
    pub period: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn adjustments_index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("products.view")?;
    let rows: Vec<(i64, String, Option<i64>, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, reference_number, stock_location_id, status, reason, reject_note, created_at
         FROM stock_adjustments ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut adjustments = Vec::new();
    for row in rows {
        adjustments.push(adjustment_json(&state, row, false).await?);
    }
    Ok(Json(json!({ "stock_adjustments": adjustments, "adjustments": adjustments })))
}

pub async fn adjustments_create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<AdjustmentBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("products.update")?;
    let items = body.items.clone().unwrap_or_default();
    if items.is_empty() {
        return Err(AppError::bad("Add at least one product."));
    }
    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(AppError::bad("Reason is required."));
    }
    let location_id = body.stock_location_id.unwrap_or(
        db::meta_get(&state.pool, "stock_location_id")
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
    );
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut tx = state.pool.begin().await?;
    let reference = crate::refs::next_adjustment_ref(&mut tx, &state.pool).await?;
    let result = sqlx::query(
        "INSERT INTO stock_adjustments(reference_number, branch_id, stock_location_id, user_id, status, reason)
         VALUES(?, ?, ?, ?, 'pending', ?)",
    )
    .bind(&reference)
    .bind(branch_id)
    .bind(location_id)
    .bind(auth.user.id)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    let id = result.last_insert_rowid();
    for item in &items {
        let name: String = sqlx::query_scalar("SELECT name FROM products WHERE id = ?")
            .bind(item.product_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::bad("A product on this adjustment is missing."))?;
        let action = match item.action.trim().to_lowercase().as_str() {
            "add" | "increase" | "in" => "add",
            _ => "remove",
        };
        if item.quantity < 1 {
            return Err(AppError::bad("Quantity must be at least 1."));
        }
        sqlx::query(
            "INSERT INTO stock_adjustment_items(adjustment_id, product_id, name, action, quantity)
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(item.product_id)
        .bind(name)
        .bind(action)
        .bind(item.quantity)
        .execute(&mut *tx)
        .await?;
    }
    outbox::enqueue(&mut tx, "adjustment.created", json!({ "adjustment_id": id })).await?;
    tx.commit().await?;
    let row = load_adjustment(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "stock.adjusted",
        format!("{} created stock adjustment {reference}", auth.user.name),
        Some(json!({ "reference": reference, "status": "pending", "reason": reason })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "stock_adjustment": adjustment_json(&state, row, true).await? })),
    ))
}

pub async fn adjustments_approve(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("products.update")?;
    let row = load_adjustment(&state, id).await?;
    if row.3 != "pending" {
        return Err(AppError::bad("Only pending adjustments can be approved."));
    }
    let location_id = row.2.unwrap_or(1);
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let items: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT product_id, action, quantity FROM stock_adjustment_items WHERE adjustment_id = ?",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let mut tx = state.pool.begin().await?;
    for (product_id, action, qty) in items {
        let delta = if action == "add" { qty } else { -qty };
        if delta < 0 {
            let on_hand: i64 = sqlx::query_scalar(
                "SELECT COALESCE(on_hand, 0) FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
            )
            .bind(product_id)
            .bind(location_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);
            if on_hand < qty {
                return Err(AppError::bad("Not enough stock to approve this adjustment."));
            }
        }
        sqlx::query(
            "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
             VALUES(?, ?, ?, ?, 0)
             ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand = on_hand + excluded.on_hand",
        )
        .bind(product_id)
        .bind(branch_id)
        .bind(location_id)
        .bind(delta)
        .execute(&mut *tx)
        .await?;
        let after: i64 = sqlx::query_scalar(
            "SELECT on_hand FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
        )
        .bind(product_id)
        .bind(location_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
             VALUES(?, ?, ?, ?, 'adjustment', ?, ?, 'stock_adjustment', ?)",
        )
        .bind(product_id)
        .bind(location_id)
        .bind(branch_id)
        .bind(auth.user.id)
        .bind(delta)
        .bind(after)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE stock_adjustments SET status = 'approved' WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    outbox::enqueue(&mut tx, "adjustment.approved", json!({ "adjustment_id": id })).await?;
    tx.commit().await?;
    let row = load_adjustment(&state, id).await?;
    let reference = row.1.clone();
    activity::log(
        &state,
        &auth,
        "stock.adjusted",
        format!("{} approved stock adjustment {reference}", auth.user.name),
        Some(json!({ "adjustment_id": id, "reference": reference, "status": "approved" })),
    )
    .await;
    Ok(Json(json!({ "stock_adjustment": adjustment_json(&state, row, true).await? })))
}

pub async fn adjustments_reject(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<RejectBody>,
) -> AppResult<Json<Value>> {
    auth.require("products.update")?;
    let row = load_adjustment(&state, id).await?;
    if row.3 != "pending" {
        return Err(AppError::bad("Only pending adjustments can be rejected."));
    }
    sqlx::query("UPDATE stock_adjustments SET status = 'rejected', reject_note = ? WHERE id = ?")
        .bind(body.note.as_deref())
        .bind(id)
        .execute(&state.pool)
        .await?;
    let row = load_adjustment(&state, id).await?;
    let reference = row.1.clone();
    activity::log(
        &state,
        &auth,
        "stock.adjusted",
        format!("{} rejected stock adjustment {reference}", auth.user.name),
        Some(json!({ "adjustment_id": id, "reference": reference, "status": "rejected" })),
    )
    .await;
    Ok(Json(json!({ "stock_adjustment": adjustment_json(&state, row, true).await? })))
}

pub async fn movements_index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<MovementQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("reports.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let mut sql = String::from(
        "SELECT m.id, m.product_id, p.name, m.stock_location_id, l.name, b.name, m.branch_id,
                m.user_id, u.name, m.kind, m.quantity, m.balance_after, m.document_type, m.document_id, m.created_at
         FROM stock_movements m
         JOIN products p ON p.id = m.product_id
         LEFT JOIN stock_locations l ON l.id = m.stock_location_id
         LEFT JOIN branches b ON b.id = COALESCE(l.branch_id, m.branch_id)
         LEFT JOIN users u ON u.id = m.user_id
         WHERE 1=1",
    );
    if let Some(pid) = query.product_id {
        sql.push_str(&format!(" AND m.product_id = {pid}"));
    }
    match query.period.as_deref() {
        Some("today") => sql.push_str(" AND date(m.created_at) = date('now')"),
        Some("week") => sql.push_str(" AND datetime(m.created_at) >= datetime('now', '-7 days')"),
        Some("month") => sql.push_str(" AND datetime(m.created_at) >= datetime('now', '-30 days')"),
        _ => {}
    }
    sql.push_str(&format!(" ORDER BY m.created_at DESC LIMIT {limit}"));
    let rows: Vec<(
        i64,
        i64,
        String,
        i64,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
        Option<String>,
        String,
        i64,
        Option<i64>,
        Option<String>,
        Option<i64>,
        String,
    )> = sqlx::query_as(&sql).fetch_all(&state.pool).await?;
    let mut stock_movements = Vec::with_capacity(rows.len());
    for (
        id,
        product_id,
        name,
        location_id,
        location_name,
        branch_name,
        branch_id,
        _user_id,
        actor,
        kind,
        quantity,
        balance_after,
        document_type,
        document_id,
        created_at,
    ) in rows
    {
        let label = match (&branch_name, &location_name) {
            (Some(b), Some(l)) => format!("{b} · {l}"),
            (None, Some(l)) => l.clone(),
            (Some(b), None) => b.clone(),
            _ => String::new(),
        };
        let document = movement_document(&state, document_type.as_deref(), document_id).await?;
        stock_movements.push(json!({
            "id": id,
            "product_id": product_id,
            "product_name": name,
            "name": name,
            "product": { "id": product_id, "name": name },
            "stock_location_id": location_id,
            "stock_location": {
                "id": location_id,
                "name": location_name,
                "branch_name": branch_name,
                "label": if label.is_empty() { Value::Null } else { json!(label) },
            },
            "branch": { "id": branch_id, "name": branch_name },
            "actor": actor,
            "kind": kind,
            "quantity": quantity,
            "balance_after": balance_after,
            "document_type": document_type,
            "document_id": document_id,
            "document": document,
            "created_at": created_at,
        }));
    }
    Ok(Json(json!({ "stock_movements": stock_movements, "movements": stock_movements, "total": stock_movements.len() })))
}

async fn movement_document(
    state: &AppState,
    document_type: Option<&str>,
    document_id: Option<i64>,
) -> AppResult<Value> {
    let (Some(kind), Some(id)) = (document_type, document_id) else {
        return Ok(Value::Null);
    };
    match kind {
        "sale" => {
            let number: Option<String> =
                sqlx::query_scalar("SELECT receipt_number FROM sales WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&state.pool)
                    .await?;
            Ok(json!({ "type": "Sale", "id": id, "number": number }))
        }
        "sale_return" => {
            let number: Option<String> = sqlx::query_scalar(
                "SELECT s.receipt_number FROM sale_returns r JOIN sales s ON s.id = r.sale_id WHERE r.id = ?",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
            Ok(json!({ "type": "SaleReturn", "id": id, "number": number.map(|n| format!("RET{n}")) }))
        }
        "stock_transfer" => {
            let row: Option<(String, i64, i64)> = sqlx::query_as(
                "SELECT reference_number, from_stock_location_id, to_stock_location_id FROM stock_transfers WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
            let Some((number, from_id, to_id)) = row else {
                return Ok(json!({ "type": "StockTransfer", "id": id }));
            };
            let from = location_label(state, from_id).await?;
            let to = location_label(state, to_id).await?;
            Ok(json!({
                "type": "StockTransfer",
                "id": id,
                "number": number,
                "from": from,
                "to": to,
            }))
        }
        "purchase" => {
            let number: Option<String> =
                sqlx::query_scalar("SELECT reference_number FROM purchases WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&state.pool)
                    .await?;
            Ok(json!({ "type": "Purchase", "id": id, "number": number }))
        }
        "stock_adjustment" => {
            let number: Option<String> =
                sqlx::query_scalar("SELECT reference_number FROM stock_adjustments WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&state.pool)
                    .await?;
            Ok(json!({ "type": "StockAdjustment", "id": id, "number": number }))
        }
        other => Ok(json!({ "type": other, "id": id })),
    }
}

async fn location_label(state: &AppState, location_id: i64) -> AppResult<Option<String>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT b.name, l.name FROM stock_locations l JOIN branches b ON b.id = l.branch_id WHERE l.id = ?",
    )
    .bind(location_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|(branch, name)| format!("{branch} · {name}")))
}

pub async fn report_valuation(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("reports.view")?;
    let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
        "SELECT p.id, p.name, p.sku, p.purchase_price,
                COALESCE((SELECT SUM(b.on_hand) FROM inventory_balances b WHERE b.product_id = p.id), 0)
         FROM products p
         ORDER BY p.name",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut total = 0.0;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, sku, cost, on_hand)| {
            let unit = cost.parse::<f64>().unwrap_or(0.0);
            let value = unit * on_hand as f64;
            total += value;
            json!({
                "id": id,
                "name": name,
                "sku": sku,
                "on_hand": on_hand,
                "unit_cost": cost,
                "value": format_money(value),
            })
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "total_value": format_money(total),
        "valuation": { "items": items, "total": format_money(total) },
    })))
}

pub async fn report_in_transit(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("reports.view")?;
    let rows: Vec<(i64, String, i64, i64, String, String)> = sqlx::query_as(
        "SELECT id, reference_number, from_stock_location_id, to_stock_location_id, status, created_at
         FROM stock_transfers WHERE status = 'in_transit' ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    let transfers: Vec<Value> = rows
        .into_iter()
        .map(|(id, reference, from_id, to_id, status, created_at)| {
            json!({
                "id": id,
                "reference_number": reference,
                "from_stock_location_id": from_id,
                "to_stock_location_id": to_id,
                "status": status,
                "created_at": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "transfers": transfers })))
}

pub async fn report_discrepancies(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("reports.view")?;
    let rows: Vec<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT t.id, t.reference_number, i.quantity, i.received_quantity
         FROM stock_transfers t
         JOIN stock_transfer_items i ON i.transfer_id = t.id
         WHERE t.status = 'completed' AND i.received_quantity < i.quantity
         ORDER BY t.created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await?;
    let discrepancies: Vec<Value> = rows
        .into_iter()
        .map(|(id, reference, quantity, received)| {
            json!({
                "id": id,
                "reference_number": reference,
                "quantity": quantity,
                "received_quantity": received,
                "short": quantity - received,
            })
        })
        .collect();
    Ok(Json(json!({ "discrepancies": discrepancies })))
}

async fn load_adjustment(
    state: &AppState,
    id: i64,
) -> AppResult<(i64, String, Option<i64>, String, String, Option<String>, String)> {
    sqlx::query_as(
        "SELECT id, reference_number, stock_location_id, status, reason, reject_note, created_at
         FROM stock_adjustments WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Adjustment not found.".into()))
}

async fn adjustment_json(
    state: &AppState,
    row: (i64, String, Option<i64>, String, String, Option<String>, String),
    detail: bool,
) -> AppResult<Value> {
    let (id, reference, location_id, status, reason, reject_note, created_at) = row;
    let mut value = json!({
        "id": id,
        "reference_number": reference,
        "stock_location_id": location_id,
        "status": status,
        "reason": reason,
        "reject_note": reject_note,
        "created_at": created_at,
    });
    if detail {
        let items: Vec<(i64, i64, String, String, i64)> = sqlx::query_as(
            "SELECT id, product_id, name, action, quantity FROM stock_adjustment_items WHERE adjustment_id = ?",
        )
        .bind(id)
        .fetch_all(&state.pool)
        .await?;
        value["items"] = items
            .into_iter()
            .map(|(iid, product_id, name, action, quantity)| {
                json!({
                    "id": iid,
                    "product_id": product_id,
                    "name": name,
                    "action": action,
                    "quantity": quantity,
                })
            })
            .collect();
    }
    Ok(value)
}
