use crate::{
    activity,
    auth::AuthUser,
    config::default_data_dir,
    db,
    error::{AppError, AppResult},
    money::{format_money, parse_money},
    outbox,
    state::AppState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Multipart, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct PurchaseBody {
    #[serde(default)]
    pub supplier_id: Option<i64>,
    #[serde(default)]
    pub stock_location_id: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub items: Option<Vec<PurchaseItemIn>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PurchaseItemIn {
    pub product_id: i64,
    pub quantity: i64,
    #[serde(default)]
    pub unit_cost: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReceiveBody {
    #[serde(default)]
    pub items: Option<Vec<ReceiveItem>>,
}

#[derive(Debug, Deserialize)]
pub struct ReceiveItem {
    pub id: i64,
    pub received_quantity: i64,
    #[serde(default)]
    #[allow(dead_code)]
    pub unit_cost: Option<String>,
}

pub async fn index(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("purchases.view")?;
    let rows: Vec<(i64, String, Option<i64>, Option<i64>, Option<i64>, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, reference_number, supplier_id, branch_id, stock_location_id, status, notes, created_at
         FROM purchases ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut purchases = Vec::new();
    for row in rows {
        purchases.push(purchase_json(&state, row, false).await?);
    }
    Ok(Json(json!({ "purchases": purchases })))
}

pub async fn open_count(State(state): State<AppState>, auth: AuthUser) -> AppResult<Json<Value>> {
    auth.require("purchases.view")?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM purchases
         WHERE status IN ('draft', 'pending', 'submitted', 'ordered', 'issued', 'partial')",
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
    auth.require("purchases.view")?;
    let row = load_purchase(&state, id).await?;
    Ok(Json(json!({ "purchase": purchase_json(&state, row, true).await? })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PurchaseBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("purchases.create")?;
    let items = body.items.clone().unwrap_or_default();
    if items.is_empty() {
        return Err(AppError::bad("Add at least one product."));
    }
    let branch_id: Option<i64> = db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok());
    let location_id: Option<i64> = body.stock_location_id.or(
        db::meta_get(&state.pool, "stock_location_id")
            .await?
            .and_then(|v| v.parse().ok()),
    );

    let mut tx = state.pool.begin().await?;
    let reference = crate::refs::next_purchase_ref(&mut tx, &state.pool).await?;
    let result = sqlx::query(
        "INSERT INTO purchases(reference_number, supplier_id, branch_id, stock_location_id, status, notes)
         VALUES(?, ?, ?, ?, 'draft', ?)",
    )
    .bind(&reference)
    .bind(body.supplier_id)
    .bind(branch_id)
    .bind(location_id)
    .bind(body.notes.as_deref())
    .execute(&mut *tx)
    .await?;
    let purchase_id = result.last_insert_rowid();

    for item in &items {
        let product: Option<(String, String, String)> = sqlx::query_as(
            "SELECT name, sku, purchase_price FROM products WHERE id = ?",
        )
        .bind(item.product_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((name, sku, price)) = product else {
            return Err(AppError::bad("A product on this order is missing."));
        };
        let cost = item.unit_cost.clone().unwrap_or(price);
        sqlx::query(
            "INSERT INTO purchase_items(purchase_id, product_id, name, sku, quantity, unit_cost)
             VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(purchase_id)
        .bind(item.product_id)
        .bind(name)
        .bind(sku)
        .bind(item.quantity)
        .bind(cost)
        .execute(&mut *tx)
        .await?;
    }
    outbox::enqueue(
        &mut tx,
        "purchase.created",
        json!({ "purchase_id": purchase_id, "reference": reference }),
    )
    .await?;
    tx.commit().await?;

    let row = load_purchase(&state, purchase_id).await?;
    activity::log(
        &state,
        &auth,
        "purchase.created",
        format!("{} created purchase {reference}", auth.user.name),
        Some(json!({ "reference": reference, "purchase_id": purchase_id })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "purchase": purchase_json(&state, row, true).await? })),
    ))
}

pub async fn submit(State(state): State<AppState>, auth: AuthUser, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    set_status(&state, &auth, id, "draft", "submitted", "purchases.create").await
}
pub async fn issue(State(state): State<AppState>, auth: AuthUser, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    set_status(&state, &auth, id, "submitted", "issued", "purchases.issue").await
}
pub async fn close(State(state): State<AppState>, auth: AuthUser, Path(id): Path<i64>) -> AppResult<Json<Value>> {
    set_status(&state, &auth, id, "received", "closed", "purchases.close").await
}
pub async fn notify(State(_state): State<AppState>, auth: AuthUser, Path(_id): Path<i64>) -> AppResult<Json<Value>> {
    auth.require("purchases.notify")?;
    Ok(Json(json!({ "ok": true, "message": "Notify is unavailable offline. The order is saved on this host." })))
}

pub async fn receive(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<ReceiveBody>,
) -> AppResult<Json<Value>> {
    auth.require("purchases.receive")?;
    let row = load_purchase(&state, id).await?;
    if row.5 != "issued" && row.5 != "submitted" {
        return Err(AppError::bad("Only issued orders can be received."));
    }
    let branch_id = row.3.unwrap_or(1);
    let location_id = row.4.unwrap_or(1);
    let items = body.items.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    for item in items {
        let line: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT id, product_id, quantity FROM purchase_items WHERE id = ? AND purchase_id = ?",
        )
        .bind(item.id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((_, product_id, ordered)) = line else {
            continue;
        };
        let qty = item.received_quantity.max(0).min(ordered);
        sqlx::query("UPDATE purchase_items SET received_quantity = ? WHERE id = ?")
            .bind(qty)
            .bind(item.id)
            .execute(&mut *tx)
            .await?;
        if qty > 0 {
            sqlx::query(
                "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
                 VALUES(?, ?, ?, ?, 0)
                 ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand = on_hand + excluded.on_hand",
            )
            .bind(product_id)
            .bind(branch_id)
            .bind(location_id)
            .bind(qty)
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
                 VALUES(?, ?, ?, ?, 'purchase', ?, ?, 'purchase', ?)",
            )
            .bind(product_id)
            .bind(location_id)
            .bind(branch_id)
            .bind(auth.user.id)
            .bind(qty)
            .bind(after)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query("UPDATE purchases SET status = 'received', updated_at = datetime('now') WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    outbox::enqueue(&mut tx, "purchase.received", json!({ "purchase_id": id })).await?;
    tx.commit().await?;
    let row = load_purchase(&state, id).await?;
    let reference = row.1.clone();
    activity::log(
        &state,
        &auth,
        "purchase.received",
        format!("{} received purchase {reference}", auth.user.name),
        Some(json!({ "purchase_id": id, "reference": reference })),
    )
    .await;
    Ok(Json(json!({ "purchase": purchase_json(&state, row, true).await? })))
}

async fn set_status(
    state: &AppState,
    auth: &AuthUser,
    id: i64,
    from: &str,
    to: &str,
    permission: &str,
) -> AppResult<Json<Value>> {
    auth.require(permission)?;
    let row = load_purchase(state, id).await?;
    if row.5 != from && !(from == "submitted" && row.5 == "draft" && to == "issued") {
        if row.5 != from {
            return Err(AppError::bad(format!("Cannot move purchase from {} to {to}.", row.5)));
        }
    }
    sqlx::query("UPDATE purchases SET status = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(to)
        .bind(id)
        .execute(&state.pool)
        .await?;
    let row = load_purchase(state, id).await?;
    let reference = row.1.clone();
    let action = match to {
        "submitted" => "purchase.submitted",
        "issued" => "purchase.issued",
        "closed" => "purchase.closed",
        "cancelled" => "purchase.cancelled",
        _ => "purchase.updated",
    };
    activity::log(
        state,
        auth,
        action,
        format!("{} moved purchase {reference} to {to}", auth.user.name),
        Some(json!({ "purchase_id": id, "reference": reference, "status": to })),
    )
    .await;
    Ok(Json(json!({ "purchase": purchase_json(state, row, true).await? })))
}

pub async fn receipt_images(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    mut multipart: Multipart,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("purchases.receive")?;
    let row = load_purchase(&state, id).await?;
    let dir = purchase_images_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::bad(format!("Could not store image: {e}")))?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("Invalid upload: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "remove_receipt_image_ids[]" || name == "remove_receipt_image_ids" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::bad(format!("Invalid upload: {e}")))?;
            if let Ok(image_id) = text.trim().parse::<i64>() {
                delete_purchase_image(&state, id, image_id).await?;
            }
            continue;
        }
        if name != "receipt_images[]" && name != "receipt_images" {
            continue;
        }
        let filename = field.file_name().unwrap_or("receipt.jpg").to_string();
        let content_type = field
            .content_type()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "image/jpeg".into());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| AppError::bad(format!("Invalid upload: {e}")))?;
        if bytes.is_empty() {
            continue;
        }
        let result = sqlx::query(
            "INSERT INTO purchase_images(purchase_id, filename, content_type) VALUES(?, ?, ?)",
        )
        .bind(id)
        .bind(&filename)
        .bind(&content_type)
        .execute(&state.pool)
        .await?;
        let image_id = result.last_insert_rowid();
        let path = image_path(image_id);
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| AppError::bad(format!("Could not store image: {e}")))?;
    }
    activity::log(
        &state,
        &auth,
        "purchase.updated",
        format!("{} updated receipt images on purchase {}", auth.user.name, row.1),
        Some(json!({ "purchase_id": id, "reference": row.1 })),
    )
    .await;
    let row = load_purchase(&state, id).await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "purchase": purchase_json(&state, row, true).await? })),
    ))
}

pub async fn show_receipt_image(
    State(state): State<AppState>,
    Path(image_id): Path<i64>,
) -> AppResult<impl IntoResponse> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT id, content_type FROM purchase_images WHERE id = ?")
            .bind(image_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((_id, content_type)) = row else {
        return Err(AppError::NotFound("Image not found.".into()));
    };
    let path = image_path(image_id);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("Image not found.".into()))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("image/jpeg")),
    );
    Ok((headers, Bytes::from(bytes)))
}

async fn load_purchase(
    state: &AppState,
    id: i64,
) -> AppResult<(i64, String, Option<i64>, Option<i64>, Option<i64>, String, Option<String>, String)> {
    sqlx::query_as(
        "SELECT id, reference_number, supplier_id, branch_id, stock_location_id, status, notes, created_at
         FROM purchases WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Purchase not found.".into()))
}

async fn purchase_json(
    state: &AppState,
    row: (i64, String, Option<i64>, Option<i64>, Option<i64>, String, Option<String>, String),
    detail: bool,
) -> AppResult<Value> {
    let (id, reference, supplier_id, branch_id, location_id, status, notes, created_at) = row;
    let supplier: Option<(String, Option<String>)> = if let Some(sid) = supplier_id {
        sqlx::query_as("SELECT COALESCE(NULLIF(company,''), name), email FROM suppliers WHERE id = ?")
            .bind(sid)
            .fetch_optional(&state.pool)
            .await?
    } else {
        None
    };
    let mut value = json!({
        "id": id,
        "reference_number": reference,
        "supplier_id": supplier_id,
        "supplier": {
            "id": supplier_id,
            "name": supplier.as_ref().map(|s| s.0.clone()).unwrap_or_else(|| "Supplier".into()),
            "email": supplier.as_ref().and_then(|s| s.1.clone()),
        },
        "branch_id": branch_id,
        "stock_location_id": location_id,
        "status": status,
        "notes": notes,
        "created_at": created_at,
        "total": "0",
        "due": "0",
    });
    if detail {
        let items: Vec<(i64, i64, String, Option<String>, i64, i64, String)> = sqlx::query_as(
            "SELECT id, product_id, name, sku, quantity, received_quantity, unit_cost FROM purchase_items WHERE purchase_id = ?",
        )
        .bind(id)
        .fetch_all(&state.pool)
        .await?;
        let mut total = 0.0;
        value["items"] = items
            .into_iter()
            .map(|(iid, product_id, name, sku, quantity, received_quantity, unit_cost)| {
                let line = parse_money(&unit_cost).unwrap_or(0.0) * quantity as f64;
                total += line;
                json!({
                    "id": iid,
                    "product_id": product_id,
                    "name": name,
                    "sku": sku,
                    "quantity": quantity,
                    "received_quantity": received_quantity,
                    "outstanding": (quantity - received_quantity).max(0),
                    "cancelled_quantity": 0,
                    "unit_cost": unit_cost,
                    "line_total": format_money(line),
                })
            })
            .collect();
        value["total"] = json!(format_money(total));
        value["due"] = json!(format_money(total));
        let images: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, filename FROM purchase_images WHERE purchase_id = ? ORDER BY id")
                .bind(id)
                .fetch_all(&state.pool)
                .await?;
        value["receipt_images"] = images
            .into_iter()
            .map(|(image_id, _filename)| {
                json!({
                    "id": image_id,
                    "url": format!("/api/v1/purchase_images/{image_id}"),
                })
            })
            .collect();
        value["receipts"] = json!([]);
    }
    Ok(value)
}

fn purchase_images_dir() -> PathBuf {
    default_data_dir().join("purchase_images")
}

fn image_path(image_id: i64) -> PathBuf {
    purchase_images_dir().join(format!("{image_id}.bin"))
}

async fn delete_purchase_image(state: &AppState, purchase_id: i64, image_id: i64) -> AppResult<()> {
    let deleted = sqlx::query("DELETE FROM purchase_images WHERE id = ? AND purchase_id = ?")
        .bind(image_id)
        .bind(purchase_id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if deleted > 0 {
        let _ = tokio::fs::remove_file(image_path(image_id)).await;
    }
    Ok(())
}
