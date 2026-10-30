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
         WHERE b.product_id = p.id AND b.product_variant_id IS NULL), 0)";
const WAREHOUSE_ON_HAND_SQL: &str = "COALESCE((SELECT SUM(b.on_hand) FROM inventory_balances b
         INNER JOIN stock_locations sl ON sl.id = b.stock_location_id AND sl.kind = 'warehouse'
         WHERE b.product_id = p.id AND b.product_variant_id IS NULL), 0)";
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
        sql.push_str(" AND (p.name LIKE ? OR p.sku LIKE ? OR IFNULL(p.barcode,'') LIKE ? OR EXISTS (SELECT 1 FROM product_barcodes pb WHERE pb.product_id = p.id AND pb.active = 1 AND (pb.barcode LIKE ? OR IFNULL(pb.description,'') LIKE ?)))");
        let like = format!("%{q}%");
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like);
    }
    sql.push_str(" AND IFNULL(p.status, 'active') = 'active'");
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
    let barcode_drafts = parse_barcode_drafts(fields.get("barcodes").map(String::as_str), draft.barcode.as_deref());
    if !barcode_drafts.is_empty() {
        replace_barcodes_tx(&mut tx, id, &barcode_drafts).await?;
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

#[derive(Debug, Deserialize)]
pub struct LookupQuery {
    pub barcode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MergeBody {
    pub primary_id: i64,
    pub source_ids: Vec<i64>,
}

pub async fn lookup(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<LookupQuery>,
) -> AppResult<Json<Value>> {
    if !auth.allow("products.view") && !auth.allow("sales") && !auth.allow("barcodes.view") {
        return Err(AppError::forbidden("You do not have access to that."));
    }
    let code = normalize_barcode(query.barcode.as_deref().unwrap_or(""));
    if code.is_empty() {
        return Err(AppError::bad("Scan or enter a barcode."));
    }
    #[derive(FromRow)]
    struct Hit {
        product_id: i64,
        product_variant_id: Option<i64>,
        barcode: String,
        description: Option<String>,
        is_primary: i64,
    }
    let hit = sqlx::query_as::<_, Hit>(
        "SELECT product_id, product_variant_id, barcode, description, is_primary
         FROM product_barcodes WHERE barcode = ? AND active = 1 LIMIT 1",
    )
    .bind(&code)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("No product matches that barcode.".into()))?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT IFNULL(status, 'active') FROM products WHERE id = ?")
            .bind(hit.product_id)
            .fetch_optional(&state.pool)
            .await?;
    if status.as_deref() != Some("active") {
        return Err(AppError::NotFound("No product matches that barcode.".into()));
    }
    let product = product_payload(&state, hit.product_id).await?;
    let variant = if let Some(vid) = hit.product_variant_id {
        variant_payload(&state, hit.product_id, vid).await?
    } else {
        Value::Null
    };
    Ok(Json(json!({
        "product": product,
        "variant": variant,
        "barcode": {
            "barcode": hit.barcode,
            "description": hit.description,
            "is_primary": hit.is_primary != 0,
            "product_variant_id": hit.product_variant_id,
        }
    })))
}

pub async fn merge(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<MergeBody>,
) -> AppResult<Json<Value>> {
    auth.require("products.merge")?;
    let primary = body.primary_id;
    let sources: Vec<i64> = body
        .source_ids
        .into_iter()
        .filter(|id| *id != primary)
        .collect();
    if sources.is_empty() {
        return Err(AppError::bad("Choose at least one product to merge."));
    }
    let primary_variants: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM product_variants WHERE product_id = ? AND active = 1",
    )
    .bind(primary)
    .fetch_one(&state.pool)
    .await?;
    if primary_variants > 0 {
        return Err(AppError::bad("Merge only works for products without variants."));
    }
    for sid in &sources {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM product_variants WHERE product_id = ? AND active = 1",
        )
        .bind(sid)
        .fetch_one(&state.pool)
        .await?;
        if n > 0 {
            return Err(AppError::bad("Merge only works for products without variants."));
        }
    }
    let store_id = db::store_location_id(&state.pool).await?;
    let mut tx = state.pool.begin().await?;
    for sid in &sources {
        sqlx::query("UPDATE product_barcodes SET product_id = ?, product_variant_id = NULL WHERE product_id = ? AND active = 1")
            .bind(primary)
            .bind(sid)
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT id, stock_location_id, on_hand FROM inventory_balances
             WHERE product_id = ? AND product_variant_id IS NULL AND on_hand > 0",
        )
        .bind(sid)
        .fetch_all(&mut *tx)
        .await?;
        for (_bid, location_id, on_hand) in rows {
            let reserved: i64 = sqlx::query_scalar(
                "SELECT reserved FROM inventory_balances WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(sid)
            .bind(location_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
            if reserved > 0 {
                return Err(AppError::bad("A source product still has reserved stock."));
            }
            sqlx::query(
                "UPDATE inventory_balances SET on_hand = 0 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(sid)
            .bind(location_id)
            .execute(&mut *tx)
            .await?;
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT on_hand FROM inventory_balances WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(primary)
            .bind(location_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(cur) = existing {
                sqlx::query(
                    "UPDATE inventory_balances SET on_hand = ? WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
                )
                .bind(cur + on_hand)
                .bind(primary)
                .bind(location_id)
                .execute(&mut *tx)
                .await?;
            } else {
                let branch_id = default_branch_id(&state).await?;
                sqlx::query(
                    "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved, product_variant_id)
                     VALUES(?, ?, ?, ?, 0, NULL)",
                )
                .bind(primary)
                .bind(branch_id)
                .bind(location_id)
                .bind(on_hand)
                .execute(&mut *tx)
                .await?;
            }
            let _ = store_id;
        }
        sqlx::query("UPDATE products SET status = 'archived', updated_at = datetime('now') WHERE id = ?")
            .bind(sid)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    activity::log(
        &state,
        &auth,
        "product.merged",
        format!("{} merged products into #{}", auth.user.name, primary),
        Some(json!({ "primary_id": primary, "source_ids": sources })),
    )
    .await;
    Ok(Json(json!({ "product": product_payload(&state, primary).await? })))
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
    let mut tx = state.pool.begin().await?;
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
    .execute(&mut *tx)
    .await?;
    if fields.contains_key("barcodes") || draft.barcode.is_some() {
        let barcode_drafts = parse_barcode_drafts(fields.get("barcodes").map(String::as_str), draft.barcode.as_deref());
        replace_barcodes_tx(&mut tx, id, &barcode_drafts).await?;
    }
    outbox::enqueue(&mut tx, "product.updated", json!({ "product_id": id })).await?;
    tx.commit().await?;
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
    let barcodes_i = idx(&["barcodes"]);
    let barcode_desc_i = idx(&["barcode_descriptions", "barcode_labels"]);
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
        let mut codes: Vec<String> = Vec::new();
        if let Some(code) = barcode_i
            .and_then(|i| cells.get(i).map(|s| normalize_barcode(s)))
            .filter(|s| !s.is_empty())
        {
            codes.push(code);
        }
        if let Some(i) = barcodes_i {
            for part in cells.get(i).map(|s| s.as_str()).unwrap_or("").split(|c| c == '|' || c == ';') {
                let code = normalize_barcode(part);
                if !code.is_empty() && !codes.iter().any(|c| c == &code) {
                    codes.push(code);
                }
            }
        }
        let descriptions: Vec<String> = barcode_desc_i
            .and_then(|i| cells.get(i).cloned())
            .unwrap_or_default()
            .split(|c| c == '|' || c == ';')
            .map(|s| s.trim().to_string())
            .collect();
        let barcode = codes.first().cloned();
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
        let drafts: Vec<BarcodeDraft> = codes
            .into_iter()
            .enumerate()
            .map(|(index, code)| BarcodeDraft {
                barcode: code,
                description: descriptions.get(index).cloned().filter(|s| !s.is_empty()),
                is_primary: index == 0,
                product_variant_id: None,
            })
            .collect();
        if let Err(e) = replace_barcodes_tx(&mut tx, id, &drafts).await {
            let _ = tx.rollback().await;
            failed.push(json!({ "row": row_num, "error": e.to_string() }));
            continue;
        }
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
    let mut payload = product_json(&row, store_id, warehouse_id);
    let barcodes = load_barcodes(state, id).await?;
    let variants = load_variants(state, id).await?;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("barcodes".into(), barcodes);
        obj.insert("variants".into(), variants);
        obj.insert("status".into(), json!("active"));
    }
    Ok(payload)
}

async fn load_barcodes(state: &AppState, product_id: i64) -> AppResult<Value> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        barcode: String,
        description: Option<String>,
        barcode_type: Option<String>,
        is_primary: i64,
        product_variant_id: Option<i64>,
        active: i64,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, barcode, description, barcode_type, is_primary, product_variant_id, active
         FROM product_barcodes WHERE product_id = ? AND active = 1 ORDER BY is_primary DESC, id",
    )
    .bind(product_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "barcode": r.barcode,
                    "description": r.description,
                    "barcode_type": r.barcode_type,
                    "is_primary": r.is_primary != 0,
                    "product_variant_id": r.product_variant_id,
                    "active": r.active != 0,
                })
            })
            .collect(),
    ))
}

async fn load_variants(state: &AppState, product_id: i64) -> AppResult<Value> {
    #[derive(FromRow)]
    struct Row {
        id: i64,
        name: String,
        sku: String,
        selling_price: String,
        purchase_price: String,
        active: i64,
        on_hand: i64,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT v.id, v.name, v.sku, v.selling_price, v.purchase_price, v.active,
                COALESCE((SELECT SUM(b.on_hand) FROM inventory_balances b
                  INNER JOIN stock_locations sl ON sl.id = b.stock_location_id AND sl.kind = 'store'
                  WHERE b.product_variant_id = v.id), 0) AS on_hand
         FROM product_variants v WHERE v.product_id = ? AND v.active = 1 ORDER BY v.id",
    )
    .bind(product_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "name": r.name,
                    "sku": r.sku,
                    "selling_price": r.selling_price,
                    "purchase_price": r.purchase_price,
                    "active": r.active != 0,
                    "stock_quantity": r.on_hand,
                    "store_on_hand": r.on_hand,
                    "warehouse_on_hand": 0,
                })
            })
            .collect(),
    ))
}

async fn variant_payload(state: &AppState, product_id: i64, variant_id: i64) -> AppResult<Value> {
    let variants = load_variants(state, product_id).await?;
    Ok(variants
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row.get("id").and_then(|v| v.as_i64()) == Some(variant_id)))
        .cloned()
        .unwrap_or(Value::Null))
}

struct BarcodeDraft {
    barcode: String,
    description: Option<String>,
    is_primary: bool,
    product_variant_id: Option<i64>,
}

fn normalize_barcode(value: &str) -> String {
    value.trim().to_string()
}

fn parse_barcode_drafts(raw: Option<&str>, fallback: Option<&str>) -> Vec<BarcodeDraft> {
    if let Some(text) = raw.filter(|s| !s.trim().is_empty()) {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) {
            let mut rows = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let code = item
                    .get("barcode")
                    .and_then(|v| v.as_str())
                    .map(normalize_barcode)
                    .unwrap_or_default();
                if code.is_empty() {
                    continue;
                }
                rows.push(BarcodeDraft {
                    barcode: code,
                    description: item
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                    is_primary: item
                        .get("is_primary")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(index == 0),
                    product_variant_id: item.get("product_variant_id").and_then(|v| v.as_i64()),
                });
            }
            if !rows.is_empty() {
                if !rows.iter().any(|row| row.is_primary) {
                    rows[0].is_primary = true;
                }
                return rows;
            }
        }
    }
    fallback
        .map(normalize_barcode)
        .filter(|s| !s.is_empty())
        .map(|barcode| {
            vec![BarcodeDraft {
                barcode,
                description: None,
                is_primary: true,
                product_variant_id: None,
            }]
        })
        .unwrap_or_default()
}

async fn replace_barcodes_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    product_id: i64,
    rows: &[BarcodeDraft],
) -> AppResult<()> {
    sqlx::query("UPDATE product_barcodes SET active = 0, is_primary = 0 WHERE product_id = ?")
        .bind(product_id)
        .execute(&mut **tx)
        .await?;
    for (index, row) in rows.iter().enumerate() {
        let code = normalize_barcode(&row.barcode);
        if code.is_empty() {
            continue;
        }
        let conflict: Option<i64> = sqlx::query_scalar(
            "SELECT product_id FROM product_barcodes WHERE barcode = ? AND active = 1 AND product_id != ? LIMIT 1",
        )
        .bind(&code)
        .bind(product_id)
        .fetch_optional(&mut **tx)
        .await?;
        if conflict.is_some() {
            return Err(AppError::bad(format!(
                "Barcode {code} is already on another product."
            )));
        }
        sqlx::query(
            "INSERT INTO product_barcodes(product_id, product_variant_id, barcode, description, is_primary, active)
             VALUES(?, ?, ?, ?, ?, 1)",
        )
        .bind(product_id)
        .bind(row.product_variant_id)
        .bind(&code)
        .bind(&row.description)
        .bind(i64::from(row.is_primary || index == 0))
        .execute(&mut **tx)
        .await?;
    }
    let primary: Option<String> = sqlx::query_scalar(
        "SELECT barcode FROM product_barcodes WHERE product_id = ? AND active = 1 ORDER BY is_primary DESC, id LIMIT 1",
    )
    .bind(product_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(code) = primary {
        sqlx::query("UPDATE products SET barcode = ? WHERE id = ?")
            .bind(code)
            .bind(product_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
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
        "barcodes": [],
        "variants": [],
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
