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
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct ProductQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub stock: Option<String>,
    #[serde(default)]
    pub stock_location_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct ProductRow {
    id: i64,
    category_id: Option<i64>,
    name: String,
    sku: String,
    barcode: Option<String>,
    description: Option<String>,
    purchase_price: String,
    selling_price: String,
    reorder_level: Option<i64>,
    featured: i64,
    /// Store (sellable) on-hand.
    on_hand: i64,
    warehouse_on_hand: i64,
}

const STORE_ON_HAND_SQL: &str = "COALESCE((SELECT SUM(b.on_hand) FROM inventory_balances b
         INNER JOIN stock_locations sl ON sl.id = b.stock_location_id AND sl.kind = 'store'
         WHERE b.product_id = p.id), 0)";
const WAREHOUSE_ON_HAND_SQL: &str = "COALESCE((SELECT SUM(b.on_hand) FROM inventory_balances b
         INNER JOIN stock_locations sl ON sl.id = b.stock_location_id AND sl.kind = 'warehouse'
         WHERE b.product_id = p.id), 0)";
pub async fn index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<ProductQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("sales") && !auth.allow("barcodes.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }

    let store_id = db::store_location_id(&state.pool).await?;
    let warehouse_id = db::warehouse_location_id(&state.pool).await?;
    let store_alert = super::merchant::meta_alert(&state, "low_stock_alert").await?;
    let warehouse_alert = super::merchant::meta_alert(&state, "warehouse_low_stock_alert").await?;
    let mut sql = format!(
        "SELECT p.id, p.category_id, p.name, p.sku, p.barcode, p.description,
                p.purchase_price, p.selling_price, p.reorder_level, p.featured,
                {STORE_ON_HAND_SQL} AS on_hand,
                {WAREHOUSE_ON_HAND_SQL} AS warehouse_on_hand
         FROM products p
         WHERE 1=1"
    );
    let mut binds: Vec<String> = vec![];
    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND (p.name LIKE ? OR p.sku LIKE ? OR IFNULL(p.barcode,'') LIKE ?)");
        let like = format!("%{q}%");
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like);
    }
    // Stock alerts / filters — store is sellable; warehouse filters for Alerts desk.
    // Thresholds are i64 from meta (safe to interpolate).
    match query.stock.as_deref() {
        Some("in_stock") => sql.push_str(&format!(
            " AND {STORE_ON_HAND_SQL} > COALESCE(p.reorder_level, {store_alert})"
        )),
        Some("out_of_stock") => sql.push_str(&format!(" AND {STORE_ON_HAND_SQL} <= 0")),
        Some("low_stock") => sql.push_str(&format!(
            " AND {STORE_ON_HAND_SQL} > 0 AND {STORE_ON_HAND_SQL} <= COALESCE(p.reorder_level, {store_alert})"
        )),
        Some("out_of_stock_warehouse") => {
            sql.push_str(&format!(" AND {WAREHOUSE_ON_HAND_SQL} <= 0"))
        }
        Some("low_stock_warehouse") => sql.push_str(&format!(
            " AND {WAREHOUSE_ON_HAND_SQL} > 0 AND {WAREHOUSE_ON_HAND_SQL} <= COALESCE(p.reorder_level, {warehouse_alert})"
        )),
        _ => {}
    }
    sql.push_str(" ORDER BY p.name LIMIT 500");

    let mut q = sqlx::query_as::<_, ProductRow>(&sql);
    for b in &binds {
        q = q.bind(b);
    }
    let rows = q.fetch_all(&state.pool).await?;
    let mut products: Vec<Value> = rows
        .iter()
        .map(|row| product_json(row, store_id, warehouse_id))
        .collect();
    if let Some(location_id) = query.stock_location_id {
        for product in &mut products {
            apply_location_stock(&state, product, location_id).await?;
        }
    }
    Ok(Json(json!({ "products": products })))
}

pub async fn alert_counts(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("sales") && !auth.allow("barcodes.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let store_alert = super::merchant::meta_alert(&state, "low_stock_alert").await?;
    let warehouse_alert = super::merchant::meta_alert(&state, "warehouse_low_stock_alert").await?;
    let store_low: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM products p
         WHERE {STORE_ON_HAND_SQL} > 0
           AND {STORE_ON_HAND_SQL} <= COALESCE(p.reorder_level, {store_alert})"
    ))
    .fetch_one(&state.pool)
    .await?;
    let warehouse_low: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM products p
         WHERE {WAREHOUSE_ON_HAND_SQL} > 0
           AND {WAREHOUSE_ON_HAND_SQL} <= COALESCE(p.reorder_level, {warehouse_alert})"
    ))
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({
        "store_low": store_low,
        "warehouse_low": warehouse_low,
        "total": store_low + warehouse_low,
    })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Query(query): Query<ProductQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("sales") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let mut product = product_payload(&state, id).await?;
    if let Some(location_id) = query.stock_location_id {
        apply_location_stock(&state, &mut product, location_id).await?;
    }
    Ok(Json(json!({ "product": product })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    multipart: Multipart,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("products.create")?;
    let fields = read_multipart(multipart).await?;
    let draft = draft_from_fields(&fields, &state).await?;
    let location_id = match fields.get("stock_location_id").and_then(|v| v.parse().ok()) {
        Some(id) => id,
        None => db::store_location_id(&state.pool).await?,
    };
    let branch_id = match fields.get("branch_id").and_then(|v| v.parse().ok()) {
        Some(id) => id,
        None => default_branch_id(&state).await?,
    };
    let qty: i64 = fields
        .get("stock_quantity")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .max(0);

    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO products(category_id, name, sku, barcode, description, purchase_price, selling_price, reorder_level, featured)
         VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(draft.category_id)
    .bind(&draft.name)
    .bind(&draft.sku)
    .bind(&draft.barcode)
    .bind(&draft.description)
    .bind(&draft.purchase_price)
    .bind(&draft.selling_price)
    .bind(draft.reorder_level)
    .bind(i64::from(draft.featured))
    .execute(&mut *tx)
    .await?;
    let id = result.last_insert_rowid();
    if qty > 0 {
        sqlx::query(
            "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
             VALUES(?, ?, ?, ?, 0)",
        )
        .bind(id)
        .bind(branch_id)
        .bind(location_id)
        .bind(qty)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
             VALUES(?, ?, ?, ?, 'receive', ?, ?, 'product', ?)",
        )
        .bind(id)
        .bind(location_id)
        .bind(branch_id)
        .bind(auth.user.id)
        .bind(qty)
        .bind(qty)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    outbox::enqueue(&mut tx, "product.created", json!({ "product_id": id })).await?;
    tx.commit().await?;
    activity::log(
        &state,
        &auth,
        "product.created",
        format!("{} added product {}", auth.user.name, draft.name),
        Some(json!({ "name": draft.name, "sku": draft.sku })),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "product": product_payload(&state, id).await? })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    multipart: Multipart,
) -> AppResult<Json<Value>> {
    auth.require("products.update")?;
    let _ = load_row(&state, id).await?;
    let fields = read_multipart(multipart).await?;
    let draft = draft_from_fields(&fields, &state).await?;
    sqlx::query(
        "UPDATE products SET category_id = ?, name = ?, sku = ?, barcode = ?, description = ?,
         purchase_price = ?, selling_price = ?, reorder_level = ?, featured = ?, updated_at = datetime('now')
         WHERE id = ?",
    )
    .bind(draft.category_id)
    .bind(&draft.name)
    .bind(&draft.sku)
    .bind(&draft.barcode)
    .bind(&draft.description)
    .bind(&draft.purchase_price)
    .bind(&draft.selling_price)
    .bind(draft.reorder_level)
    .bind(i64::from(draft.featured))
    .bind(id)
    .execute(&state.pool)
    .await?;
    activity::log(
        &state,
        &auth,
        "product.updated",
        format!("Updated product {}", draft.name),
        Some(json!({ "name": draft.name, "sku": draft.sku })),
    )
    .await;
    Ok(Json(json!({ "product": product_payload(&state, id).await? })))
}

pub async fn destroy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require("products.delete")?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM products WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    sqlx::query("DELETE FROM inventory_balances WHERE product_id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?;
    let changed = sqlx::query("DELETE FROM products WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(AppError::NotFound("Product not found.".into()));
    }
    if let Some(name) = name {
        activity::log(
            &state,
            &auth,
            "product.deleted",
            format!("{} deleted product {name}", auth.user.name),
            Some(json!({ "name": name })),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct RestockBody {
    pub quantity: i64,
}

pub async fn restock(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<RestockBody>,
) -> AppResult<Json<Value>> {
    auth.require("products.update")?;
    if body.quantity < 1 {
        return Err(AppError::bad("Quantity must be at least 1."));
    }
    let _ = load_row(&state, id).await?;
    let location_id = db::store_location_id(&state.pool).await?;
    let branch_id = default_branch_id(&state).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
         VALUES(?, ?, ?, ?, 0)
         ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand = on_hand + excluded.on_hand",
    )
    .bind(id)
    .bind(branch_id)
    .bind(location_id)
    .bind(body.quantity)
    .execute(&mut *tx)
    .await?;
    let after: i64 = sqlx::query_scalar(
        "SELECT on_hand FROM inventory_balances WHERE product_id = ? AND stock_location_id = ?",
    )
    .bind(id)
    .bind(location_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO stock_movements(product_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
         VALUES(?, ?, ?, ?, 'restock', ?, ?, 'product', ?)",
    )
    .bind(id)
    .bind(location_id)
    .bind(branch_id)
    .bind(auth.user.id)
    .bind(body.quantity)
    .bind(after)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    outbox::enqueue(&mut tx, "product.restocked", json!({ "product_id": id, "quantity": body.quantity })).await?;
    tx.commit().await?;
    let row = load_row(&state, id).await?;
    activity::log(
        &state,
        &auth,
        "stock.restocked",
        format!(
            "Restocked {} by {}. On hand now {after}",
            row.name, body.quantity
        ),
        Some(json!({
            "product_id": id,
            "quantity": body.quantity,
            "on_hand_after": after,
        })),
    )
    .await;
    Ok(Json(json!({ "product": product_payload(&state, id).await? })))
}

pub async fn import(
    State(state): State<AppState>,
    auth: AuthUser,
    mut multipart: Multipart,
) -> AppResult<Json<Value>> {
    auth.require("products.create")?;
    let mut csv_bytes: Option<Vec<u8>> = None;
    let mut location_id = db::store_location_id(&state.pool).await?;
    let mut zero_quantities = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("Invalid upload: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::bad(format!("Invalid upload: {e}")))?;
        match name.as_str() {
            "file" => csv_bytes = Some(data.to_vec()),
            "stock_location_id" => {
                if let Ok(text) = String::from_utf8(data.to_vec()) {
                    if let Ok(id) = text.trim().parse::<i64>() {
                        location_id = id;
                    }
                }
            }
            "zero_quantities" => {
                if let Ok(text) = String::from_utf8(data.to_vec()) {
                    zero_quantities = matches!(text.trim().to_lowercase().as_str(), "1" | "true" | "yes");
                }
            }
            _ => {}
        }
    }
    let bytes = csv_bytes.ok_or_else(|| AppError::bad("Choose a CSV file to import."))?;
    let text = String::from_utf8_lossy(&bytes);
    let branch_id = default_branch_id(&state).await?;
    let mut imported = 0i64;
    let mut failed: Vec<Value> = vec![];
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("").to_lowercase();
    let cols = split_csv(&header);
    let idx = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| cols.iter().position(|c| c.trim() == *n))
    };
    let name_i = idx(&["name", "product", "product name"]).unwrap_or(0);
    let sku_i = idx(&["sku", "code"]);
    let barcode_i = idx(&["barcode", "ean", "upc"]);
    let purchase_i = idx(&["purchase_price", "cost", "buy"]);
    let sell_i = idx(&["selling_price", "price", "sell"]);
    let qty_i = idx(&["stock_quantity", "quantity", "qty", "stock"]);
    let cat_i = idx(&["category", "category_name"]);

    for (row_no, line) in lines.enumerate() {
        let row_num = (row_no + 2) as i64;
        if line.trim().is_empty() {
            continue;
        }
        let cells = split_csv(line);
        let name = cells.get(name_i).map(|s| s.trim()).unwrap_or("");
        if name.is_empty() {
            failed.push(json!({ "row": row_num, "error": "Name is required." }));
            continue;
        }
        let sku = match sku_i
            .and_then(|i| cells.get(i).map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
        {
            Some(sku) => sku,
            None => match generate_sku(&state).await {
                Ok(sku) => sku,
                Err(e) => {
                    failed.push(json!({ "row": row_num, "error": e.to_string() }));
                    continue;
                }
            },
        };
        let barcode = barcode_i.and_then(|i| cells.get(i).map(|s| s.trim().to_string())).filter(|s| !s.is_empty());
        let purchase = purchase_i
            .and_then(|i| cells.get(i).map(|s| s.trim().to_string()))
            .unwrap_or_else(|| "0".into());
        let selling = sell_i
            .and_then(|i| cells.get(i).map(|s| s.trim().to_string()))
            .unwrap_or_else(|| "0".into());
        let qty = if zero_quantities {
            0
        } else {
            qty_i
                .and_then(|i| cells.get(i).and_then(|s| s.trim().parse::<i64>().ok()))
                .unwrap_or(0)
                .max(0)
        };
        let category_id = if let Some(i) = cat_i {
            let cat_name = cells.get(i).map(|s| s.trim()).unwrap_or("");
            if cat_name.is_empty() {
                None
            } else {
                match ensure_category(&state, cat_name).await {
                    Ok(id) => Some(id),
                    Err(e) => {
                        failed.push(json!({ "row": row_num, "error": e.to_string() }));
                        continue;
                    }
                }
            }
        } else {
            None
        };

        let mut tx = match state.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                failed.push(json!({ "row": row_num, "error": e.to_string() }));
                continue;
            }
        };
        let insert = sqlx::query(
            "INSERT INTO products(category_id, name, sku, barcode, purchase_price, selling_price)
             VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(category_id)
        .bind(name)
        .bind(&sku)
        .bind(&barcode)
        .bind(purchase)
        .bind(selling)
        .execute(&mut *tx)
        .await;
        let Ok(result) = insert else {
            let _ = tx.rollback().await;
            failed.push(json!({ "row": row_num, "error": "Could not create product (duplicate SKU?)." }));
            continue;
        };
        let id = result.last_insert_rowid();
        if qty > 0 {
            let _ = sqlx::query(
                "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
                 VALUES(?, ?, ?, ?, 0)",
            )
            .bind(id)
            .bind(branch_id)
            .bind(location_id)
            .bind(qty)
            .execute(&mut *tx)
            .await;
        }
        let _ = outbox::enqueue(&mut tx, "product.imported", json!({ "product_id": id })).await;
        if tx.commit().await.is_ok() {
            imported += 1;
        } else {
            failed.push(json!({ "row": row_num, "error": "Could not save product." }));
        }
    }

    activity::log(
        &state,
        &auth,
        "product.imported",
        format!(
            "{} imported {imported} products ({} failed)",
            auth.user.name,
            failed.len()
        ),
        Some(json!({ "imported": imported, "failed": failed.len() })),
    )
    .await;

    Ok(Json(json!({ "imported": imported, "failed": failed })))
}

struct Draft {
    name: String,
    sku: String,
    barcode: Option<String>,
    description: Option<String>,
    category_id: Option<i64>,
    purchase_price: String,
    selling_price: String,
    reorder_level: Option<i64>,
    featured: bool,
}

async fn draft_from_fields(fields: &HashMap<String, String>, state: &AppState) -> AppResult<Draft> {
    let name = fields
        .get("name")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad("Name is required."))?
        .to_string();
    let sku = match fields
        .get("sku")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    {
        Some(sku) => sku,
        None => generate_sku(state).await?,
    };
    Ok(Draft {
        name,
        sku,
        barcode: fields.get("barcode").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        description: fields.get("description").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        category_id: fields.get("category_id").and_then(|s| s.parse().ok()).filter(|&id| id > 0),
        purchase_price: fields
            .get("purchase_price")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".into()),
        selling_price: fields
            .get("selling_price")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".into()),
        reorder_level: fields.get("reorder_level").and_then(|s| s.parse().ok()),
        featured: matches!(
            fields.get("featured").map(|s| s.trim().to_lowercase()).as_deref(),
            Some("1" | "true" | "yes" | "on")
        ),
    })
}

/// Same shape as cloud `Product#generated_sku`: merchant 3-letter prefix + 5 hex chars.
async fn generate_sku(state: &AppState) -> AppResult<String> {
    use rand::Rng;
    let merchant = db::meta_get(&state.pool, "merchant_name")
        .await?
        .unwrap_or_default();
    let letters: String = merchant
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let mut prefix: String = letters.chars().take(3).collect();
    while prefix.len() < 3 {
        prefix.push('X');
    }
    for _ in 0..32 {
        let n: u32 = rand::rng().random_range(0..0x10_0000);
        let candidate = format!("{prefix}{n:05X}");
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM products WHERE sku = ?")
            .bind(&candidate)
            .fetch_optional(&state.pool)
            .await?;
        if exists.is_none() {
            return Ok(candidate);
        }
    }
    Err(AppError::bad("Could not generate a unique SKU."))
}

async fn apply_location_stock(state: &AppState, product: &mut Value, location_id: i64) -> AppResult<()> {
    let product_id = product
        .get("id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| AppError::bad("Invalid product."))?;
    let on_hand: i64 = sqlx::query_scalar(
        "SELECT COALESCE(on_hand, 0) FROM inventory_balances
         WHERE product_id = ? AND stock_location_id = ?",
    )
    .bind(product_id)
    .bind(location_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or(0);
    if let Some(obj) = product.as_object_mut() {
        obj.insert("stock_quantity".into(), json!(on_hand));
        obj.insert("stock_available".into(), json!(on_hand));
        obj.insert(
            "balances".into(),
            json!([{
                "stock_location_id": location_id,
                "on_hand": on_hand,
                "reserved": 0,
                "available": on_hand,
            }]),
        );
    }
    Ok(())
}

async fn read_multipart(mut multipart: Multipart) -> AppResult<HashMap<String, String>> {
    let mut fields = HashMap::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name.is_empty() || name.starts_with("images") || name.starts_with("remove_image") {
            continue;
        }
        let text = field
            .text()
            .await
            .map_err(|e| AppError::bad(format!("Invalid form: {e}")))?;
        fields.insert(name, text);
    }
    Ok(fields)
}

async fn ensure_category(state: &AppState, name: &str) -> AppResult<i64> {
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM categories WHERE lower(name) = lower(?)")
        .bind(name)
        .fetch_optional(&state.pool)
        .await?
    {
        return Ok(id);
    }
    let result = sqlx::query("INSERT INTO categories(name) VALUES(?)")
        .bind(name)
        .execute(&state.pool)
        .await?;
    Ok(result.last_insert_rowid())
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out
}

async fn load_row(state: &AppState, id: i64) -> AppResult<ProductRow> {
    sqlx::query_as::<_, ProductRow>(&format!(
        "SELECT p.id, p.category_id, p.name, p.sku, p.barcode, p.description,
                p.purchase_price, p.selling_price, p.reorder_level, p.featured,
                {STORE_ON_HAND_SQL} AS on_hand,
                {WAREHOUSE_ON_HAND_SQL} AS warehouse_on_hand
         FROM products p
         WHERE p.id = ?"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Product not found.".into()))
}

async fn default_branch_id(state: &AppState) -> AppResult<i64> {
    Ok(db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1))
}

async fn product_payload(state: &AppState, id: i64) -> AppResult<Value> {
    let row = load_row(state, id).await?;
    let store_id = db::store_location_id(&state.pool).await?;
    let warehouse_id = db::warehouse_location_id(&state.pool).await?;
    Ok(product_json(&row, store_id, warehouse_id))
}

fn product_json(row: &ProductRow, store_id: i64, warehouse_id: i64) -> Value {
    json!({
        "id": row.id,
        "name": row.name,
        "sku": row.sku,
        "barcode": row.barcode,
        "description": row.description,
        "category_id": row.category_id,
        "purchase_price": row.purchase_price,
        "selling_price": row.selling_price,
        "stock_quantity": row.on_hand,
        "store_on_hand": row.on_hand,
        "warehouse_on_hand": row.warehouse_on_hand,
        "reorder_level": row.reorder_level,
        "featured": row.featured != 0,
        "images": [],
        "balances": [
            {
                "stock_location_id": store_id,
                "kind": "store",
                "on_hand": row.on_hand,
                "reserved": 0,
                "available": row.on_hand,
            },
            {
                "stock_location_id": warehouse_id,
                "kind": "warehouse",
                "on_hand": row.warehouse_on_hand,
                "reserved": 0,
                "available": row.warehouse_on_hand,
            },
        ],
        "unit_value": row.purchase_price,
        "stock_value": format_money(
            row.purchase_price.parse::<f64>().unwrap_or(0.0) * row.on_hand as f64
        ),
    })
}
