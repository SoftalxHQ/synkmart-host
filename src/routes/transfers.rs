use crate::{
    activity,
    auth::AuthUser,
    error::{AppError, AppResult},
    outbox,
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
#[allow(dead_code)]
pub struct TransferBody {
    pub from_stock_location_id: i64,
    pub to_stock_location_id: i64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub items: Option<Vec<TransferItemIn>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct TransferItemIn {
    pub product_id: i64,
    pub quantity: i64,
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub received_quantity: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TransferUpdateBody {
    #[serde(default)]
    pub from_stock_location_id: Option<i64>,
    #[serde(default)]
    pub to_stock_location_id: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub items: Option<Vec<TransferItemPatch>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct TransferItemPatch {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub product_id: Option<i64>,
    #[serde(default)]
    pub quantity: Option<i64>,
    #[serde(default)]
    pub received_quantity: Option<i64>,
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("transfers.view")?;
    let rows: Vec<(i64, String, i64, i64, String, String, String)> = sqlx::query_as(
        "SELECT id, reference_number, from_stock_location_id, to_stock_location_id, status, scope, created_at
         FROM stock_transfers ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut transfers = Vec::new();
    for row in rows {
        transfers.push(transfer_json(&state, row, false).await?);
    }
    Ok(Json(json!({ "stock_transfers": transfers })))
}

pub async fn open_count(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("transfers.view")?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stock_transfers WHERE status IN ('pending', 'in_transit')",
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({ "total": total })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("transfers.view")?;
    let row = load(&state, id).await?;
    Ok(Json(json!({ "stock_transfer": transfer_json(&state, row, true).await? })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TransferBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("transfers.create")?;
    if body.from_stock_location_id == body.to_stock_location_id {
        return Err(AppError::bad("Choose different From and To locations."));
    }
    let items = body.items.clone().unwrap_or_default();
    if items.is_empty() {
        return Err(AppError::bad("Add at least one product."));
    }
    let status = body.status.as_deref().unwrap_or("pending");
    let mut tx = state.pool.begin().await?;
    let reference = crate::refs::next_transfer_ref(&mut tx, &state.pool).await?;
    let result = sqlx::query(
        "INSERT INTO stock_transfers(reference_number, from_stock_location_id, to_stock_location_id, status, scope)
         VALUES(?, ?, ?, ?, 'internal')",
    )
    .bind(&reference)
    .bind(body.from_stock_location_id)
    .bind(body.to_stock_location_id)
    .bind(status)
    .execute(&mut *tx)
    .await?;
    let transfer_id = result.last_insert_rowid();
    for item in &items {
        let name: String = sqlx::query_scalar("SELECT name FROM products WHERE id = ?")
            .bind(item.product_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::bad("A product on this transfer is missing."))?;
        let sku: Option<String> = sqlx::query_scalar("SELECT sku FROM products WHERE id = ?")
            .bind(item.product_id)
            .fetch_one(&mut *tx)
            .await
            .ok();
        sqlx::query(
            "INSERT INTO stock_transfer_items(transfer_id, product_id, name, sku, quantity)
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(transfer_id)
        .bind(item.product_id)
        .bind(name)
        .bind(sku)
        .bind(item.quantity)
        .execute(&mut *tx)
        .await?;
    }
    if status == "in_transit" {
        move_out(&mut tx, transfer_id, body.from_stock_location_id, auth.user.id).await?;
    }
    outbox::enqueue(&mut tx, "transfer.created", json!({ "transfer_id": transfer_id })).await?;
    tx.commit().await?;
    let row = load(&state, transfer_id).await?;
    activity::log(
        &state,
        &auth,
        "stock.transferred",
        format!("{} created stock transfer {reference} ({status})", auth.user.name),
        Some(json!({
            "transfer_id": transfer_id,
            "reference": reference,
            "status": status,
        })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "stock_transfer": transfer_json(&state, row, true).await? })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<TransferUpdateBody>,
) -> AppResult<Json<Value>> {
    let row = load(&state, id).await?;
    let current = row.4.clone();
    let mut tx = state.pool.begin().await?;

    if let Some(status) = body.status.as_deref() {
        match (current.as_str(), status) {
            ("pending", "in_transit") => {
                auth.require("transfers.move")?;
                move_out(&mut tx, id, row.2, auth.user.id).await?;
                sqlx::query("UPDATE stock_transfers SET status = 'in_transit', updated_at = datetime('now') WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            ("pending", "cancelled") | ("in_transit", "cancelled") => {
                auth.require("transfers.cancel")?;
                if current == "in_transit" {
                    recall_to_source(&mut tx, id, row.2, auth.user.id).await?;
                }
                sqlx::query(
                    "UPDATE stock_transfers SET status = 'cancelled', cancel_reason = ?, updated_at = datetime('now') WHERE id = ?",
                )
                .bind(body.reason.as_deref())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            ("in_transit", "completed") => {
                auth.require("transfers.receive")?;
                complete_receive(
                    &mut tx,
                    id,
                    row.3,
                    auth.user.id,
                    body.items.clone().unwrap_or_default(),
                )
                .await?;
                sqlx::query("UPDATE stock_transfers SET status = 'completed', updated_at = datetime('now') WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            ("pending", "pending") => {
                auth.require("transfers.create")?;
                // allow editing lines omitted for brevity
            }
            _ => return Err(AppError::bad(format!("Cannot change transfer from {current} to {status}."))),
        }
    } else {
        auth.require("transfers.create")?;
    }
    outbox::enqueue(&mut tx, "transfer.updated", json!({ "transfer_id": id })).await?;
    tx.commit().await?;
    let row = load(&state, id).await?;
    let reference = row.1.clone();
    let status = row.4.clone();
    activity::log(
        &state,
        &auth,
        "stock.transferred",
        format!(
            "{} updated stock transfer {reference} to {status}",
            auth.user.name
        ),
        Some(json!({
            "transfer_id": id,
            "reference": reference,
            "status": status,
        })),
    )
    .await;
    Ok(Json(json!({ "stock_transfer": transfer_json(&state, row, true).await? })))
}

async fn move_out(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    transfer_id: i64,
    from_location: i64,
    user_id: i64,
) -> AppResult<()> {
    let items: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT product_id, quantity FROM stock_transfer_items WHERE transfer_id = ?",
    )
    .bind(transfer_id)
    .fetch_all(&mut **tx)
    .await?;
    for (product_id, qty) in items {
        let on_hand: i64 = sqlx::query_scalar(
            "SELECT COALESCE(on_hand, 0) FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
        )
        .bind(product_id)
        .bind(from_location)
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or(0);
        if on_hand < qty {
            return Err(AppError::bad("Not enough stock at the source location."));
        }
        sqlx::query(
            "UPDATE inventory_balances SET on_hand = on_hand - ? WHERE product_id = ? AND stock_location_id = ?",
        )
        .bind(qty)
        .bind(product_id)
        .bind(from_location)
        .execute(&mut **tx)
        .await?;
        let after = on_hand - qty;
        sqlx::query(
            "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
             VALUES(?, ?, 1, ?, 'transfer_out', ?, ?, 'stock_transfer', ?)",
        )
        .bind(product_id)
        .bind(from_location)
        .bind(user_id)
        .bind(-qty)
        .bind(after)
        .bind(transfer_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn recall_to_source(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    transfer_id: i64,
    from_location: i64,
    user_id: i64,
) -> AppResult<()> {
    let items: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT product_id, quantity FROM stock_transfer_items WHERE transfer_id = ?",
    )
    .bind(transfer_id)
    .fetch_all(&mut **tx)
    .await?;
    for (product_id, qty) in items {
        sqlx::query(
            "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
             VALUES(?, 1, ?, ?, 0)
             ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand = on_hand + excluded.on_hand",
        )
        .bind(product_id)
        .bind(from_location)
        .bind(qty)
        .execute(&mut **tx)
        .await?;
        let after: i64 = sqlx::query_scalar(
            "SELECT on_hand FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
        )
        .bind(product_id)
        .bind(from_location)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
             VALUES(?, ?, 1, ?, 'transfer_recall', ?, ?, 'stock_transfer', ?)",
        )
        .bind(product_id)
        .bind(from_location)
        .bind(user_id)
        .bind(qty)
        .bind(after)
        .bind(transfer_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn complete_receive(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    transfer_id: i64,
    to_location: i64,
    user_id: i64,
    receives: Vec<TransferItemPatch>,
) -> AppResult<()> {
    let items: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT id, product_id, quantity FROM stock_transfer_items WHERE transfer_id = ?",
    )
    .bind(transfer_id)
    .fetch_all(&mut **tx)
    .await?;
    for (item_id, product_id, qty) in items {
        let received = receives
            .iter()
            .find(|r| r.id == Some(item_id) || r.product_id == Some(product_id))
            .and_then(|r| r.received_quantity)
            .unwrap_or(qty)
            .clamp(0, qty);
        sqlx::query("UPDATE stock_transfer_items SET received_quantity = ? WHERE id = ?")
            .bind(received)
            .bind(item_id)
            .execute(&mut **tx)
            .await?;
        if received > 0 {
            sqlx::query(
                "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
                 VALUES(?, 1, ?, ?, 0)
                 ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand = on_hand + excluded.on_hand",
            )
            .bind(product_id)
            .bind(to_location)
            .bind(received)
            .execute(&mut **tx)
            .await?;
            let after: i64 = sqlx::query_scalar(
                "SELECT on_hand FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
            )
            .bind(product_id)
            .bind(to_location)
            .fetch_one(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
                 VALUES(?, ?, 1, ?, 'transfer_in', ?, ?, 'stock_transfer', ?)",
            )
            .bind(product_id)
            .bind(to_location)
            .bind(user_id)
            .bind(received)
            .bind(after)
            .bind(transfer_id)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

async fn load(
    state: &AppState,
    id: i64,
) -> AppResult<(i64, String, i64, i64, String, String, String)> {
    sqlx::query_as(
        "SELECT id, reference_number, from_stock_location_id, to_stock_location_id, status, scope, created_at
         FROM stock_transfers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Transfer not found.".into()))
}

async fn on_hand(state: &AppState, product_id: i64, stock_location_id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(on_hand, 0) FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
    )
    .bind(product_id)
    .bind(stock_location_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or(0))
}

async fn transfer_json(
    state: &AppState,
    row: (i64, String, i64, i64, String, String, String),
    detail: bool,
) -> AppResult<Value> {
    let (id, reference, from_id, to_id, status, scope, created_at) = row;
    let from_name: String = location_label(state, from_id).await;
    let to_name: String = location_label(state, to_id).await;
    let mut value = json!({
        "id": id,
        "reference_number": reference,
        "status": status,
        "scope": scope,
        "created_at": created_at,
        "from_stock_location": { "id": from_id, "label": from_name, "name": from_name },
        "to_stock_location": { "id": to_id, "label": to_name, "name": to_name },
        "from_branch": { "name": from_name },
        "to_branch": { "name": to_name },
    });
    if detail {
        let items: Vec<(i64, i64, String, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT id, product_id, name, sku, quantity, received_quantity FROM stock_transfer_items WHERE transfer_id = ?",
        )
        .bind(id)
        .fetch_all(&state.pool)
        .await?;
        let mut item_json = Vec::with_capacity(items.len());
        for (iid, product_id, name, sku, quantity, received_quantity) in items {
            let from_after = on_hand(state, product_id, from_id).await?;
            let to_after = on_hand(state, product_id, to_id).await?;
            let from_before = if status == "pending" || status == "cancelled" {
                from_after
            } else {
                from_after + quantity
            };
            let to_before = if status == "completed" {
                to_after - received_quantity
            } else {
                to_after
            };
            let short = if status == "completed" {
                (quantity - received_quantity).max(0)
            } else {
                0
            };
            item_json.push(json!({
                "id": iid,
                "product_id": product_id,
                "name": name,
                "sku": sku,
                "quantity": quantity,
                "received_quantity": received_quantity,
                "short": short,
                "from_before": from_before,
                "from_after": from_after,
                "to_before": to_before,
                "to_after": to_after,
            }));
        }
        value["stock_transfer_items"] = Value::Array(item_json.clone());
        value["items"] = Value::Array(item_json);
    }
    Ok(value)
}

async fn location_label(state: &AppState, id: i64) -> String {
    sqlx::query_as::<_, (String, String)>(
        "SELECT b.name, l.name FROM stock_locations l JOIN branches b ON b.id = l.branch_id WHERE l.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .map(|(b, l)| format!("{b} · {l}"))
    .unwrap_or_else(|| format!("Location {id}"))
}
