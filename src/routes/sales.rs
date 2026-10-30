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
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;

#[derive(Debug, Deserialize)]
pub struct SaleQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub period: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSale {
    #[serde(default)]
    pub items: Option<Vec<SaleItemIn>>,
    #[serde(default)]
    pub sale_items: Option<Vec<SaleItemIn>>,
    #[serde(default)]
    pub payments: Option<Vec<PaymentIn>>,
    #[serde(default)]
    pub sale_payments: Option<Vec<PaymentIn>>,
    #[serde(default)]
    pub customer_name: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaleItemIn {
    pub product_id: i64,
    pub quantity: i64,
    #[serde(default)]
    pub product_variant_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaymentIn {
    pub method: String,
    #[serde(deserialize_with = "de_amount")]
    pub amount: String,
    #[serde(default, deserialize_with = "de_opt_amount")]
    pub tendered: Option<String>,
}

fn de_amount<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        S(String),
        N(f64),
        I(i64),
    }
    match Raw::deserialize(deserializer)? {
        Raw::S(s) => Ok(s),
        Raw::N(n) => Ok(format!("{n:.2}")),
        Raw::I(n) => Ok(format!("{n:.2}")),
    }
}

fn de_opt_amount<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    use serde::Deserialize;
    Ok(Option::<serde_json::Value>::deserialize(deserializer)?.map(|v| match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }))
}

#[derive(Debug, FromRow)]
struct SaleRow {
    id: i64,
    receipt_number: String,
    user_id: i64,
    customer_name: Option<String>,
    cashier_name: String,
    subtotal: String,
    discount: String,
    tax: String,
    total: String,
    change_amount: String,
    status: String,
    created_at: String,
}

pub async fn index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<SaleQuery>,
) -> AppResult<Json<Value>> {
    auth.require("history.view")?;
    let mut sql = String::from(
        "SELECT id, receipt_number, user_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at
         FROM sales WHERE 1=1",
    );
    if !auth.is_admin() && !auth.allow("history.view_all") {
        sql.push_str(&format!(" AND user_id = {}", auth.user.id));
    }
    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let like = q.replace('\'', "''");
        sql.push_str(&format!(
            " AND (receipt_number LIKE '%{like}%' OR IFNULL(customer_name,'') LIKE '%{like}%')"
        ));
    }
    match query.period.as_deref() {
        Some("today") => sql.push_str(" AND date(created_at) = date('now')"),
        Some("week") => sql.push_str(" AND datetime(created_at) >= datetime('now', '-7 days')"),
        Some("month") => sql.push_str(" AND datetime(created_at) >= datetime('now', '-30 days')"),
        _ => {}
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT 100");

    let rows = sqlx::query_as::<_, SaleRow>(&sql)
        .fetch_all(&state.pool)
        .await?;

    let mut sales = Vec::new();
    for row in &rows {
        sales.push(sale_json(&state, row, false).await?);
    }
    Ok(Json(json!({ "sales": sales, "summary": {} })))
}

pub async fn show(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> AppResult<Json<Value>> {
    auth.require("history.view")?;
    let row = sqlx::query_as::<_, SaleRow>(
        "SELECT id, receipt_number, user_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at
         FROM sales WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Sale not found.".into()))?;

    if !auth.is_admin() && !auth.allow("history.view_all") && row.user_id != auth.user.id {
        return Err(AppError::forbidden("You do not have access to that."));
    }

    Ok(Json(json!({ "sale": sale_json(&state, &row, true).await? })))
}

pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateSale>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("sales")?;

    let idem = body
        .idempotency_key
        .clone()
        .or_else(|| {
            headers
                .get("Idempotency-Key")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .filter(|s| !s.trim().is_empty());

    if let Some(key) = &idem {
        if let Some(cached) = sqlx::query_scalar::<_, String>(
            "SELECT response_json FROM idempotency_keys WHERE key = ?",
        )
        .bind(key)
        .fetch_optional(&state.pool)
        .await?
        {
            let value: Value = serde_json::from_str(&cached).unwrap_or(json!({}));
            return Ok((StatusCode::CREATED, Json(value)));
        }
    }

    let open: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM register_sessions WHERE user_id = ? AND status = 'open' LIMIT 1",
    )
    .bind(auth.user.id)
    .fetch_optional(&state.pool)
    .await?;
    if open.is_none() {
        return Err(AppError::bad("Open the register before taking a sale."));
    }

    let items_in = body
        .items
        .clone()
        .or(body.sale_items.clone())
        .unwrap_or_default();
    let payments_in = body
        .payments
        .clone()
        .or(body.sale_payments.clone())
        .unwrap_or_default();
    if items_in.is_empty() {
        return Err(AppError::bad("Add at least one product."));
    }
    if payments_in.is_empty() {
        return Err(AppError::bad("Add a payment."));
    }

    // Cashiers sell only from store stock — warehouse must be transferred first.
    let store_location = db::store_location_id(&state.pool).await?;
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let enable_tax = db::meta_get(&state.pool, "enable_tax")
        .await?
        .as_deref()
        == Some("1");
    let tax_rate: f64 = db::meta_get(&state.pool, "tax_rate")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);

    let mut built = Vec::new();
    let location_id = store_location;
    for item in &items_in {
        if item.quantity < 1 {
            return Err(AppError::bad("Quantity must be at least 1."));
        }
        let product: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT id, name, sku, selling_price FROM products WHERE id = ?",
        )
        .bind(item.product_id)
        .fetch_optional(&state.pool)
        .await?;
        let Some((id, mut name, mut sku, mut price_s)) = product else {
            return Err(AppError::bad("A product in the cart is no longer on this register."));
        };
        let variant_id = item.product_variant_id;
        if let Some(vid) = variant_id {
            let variant: Option<(String, String, String)> = sqlx::query_as(
                "SELECT name, sku, selling_price FROM product_variants WHERE id = ? AND product_id = ? AND active = 1",
            )
            .bind(vid)
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
            let Some((vname, vsku, vprice)) = variant else {
                return Err(AppError::bad("A variant in the cart is no longer on this register."));
            };
            name = format!("{name} · {vname}");
            sku = vsku;
            price_s = vprice;
        }
        let on_hand: i64 = if let Some(vid) = variant_id {
            sqlx::query_scalar(
                "SELECT COALESCE(on_hand, 0) FROM inventory_balances
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id = ?",
            )
            .bind(id)
            .bind(store_location)
            .bind(vid)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or(0)
        } else {
            sqlx::query_scalar(
                "SELECT COALESCE(on_hand, 0) FROM inventory_balances
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(id)
            .bind(store_location)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or(0)
        };
        if on_hand < item.quantity {
            return Err(AppError::bad(format!(
                "Not enough store stock for {name}. Transfer from warehouse first."
            )));
        }
        let unit: f64 = price_s.parse().unwrap_or(0.0);
        let line_total = (unit * item.quantity as f64 * 100.0).round() / 100.0;
        built.push((id, name, sku, unit, item.quantity, line_total, store_location, variant_id));
    }

    let subtotal: f64 = built.iter().map(|i| i.5).sum::<f64>();
    let discount = 0.0;
    let taxable = subtotal - discount;
    let tax = if enable_tax {
        ((taxable * tax_rate / 100.0) * 100.0).round() / 100.0
    } else {
        0.0
    };
    let total = ((taxable + tax) * 100.0).round() / 100.0;

    let mut pay_rows = Vec::new();
    let mut paid = 0.0;
    let mut tendered_sum = 0.0;
    for payment in &payments_in {
        let amount = parse_money(&payment.amount)?;
        let tendered = payment
            .tendered
            .as_deref()
            .map(parse_money)
            .transpose()?
            .unwrap_or(amount);
        if amount <= 0.0 {
            return Err(AppError::bad("Payment amount must be greater than 0."));
        }
        let method = payment.method.trim().to_lowercase();
        if !matches!(method.as_str(), "cash" | "card" | "transfer" | "credit") {
            return Err(AppError::bad("Unknown payment method."));
        }
        paid += amount;
        tendered_sum += tendered;
        pay_rows.push((method, amount, tendered));
    }
    if (paid - total).abs() > 0.01 {
        return Err(AppError::bad("Payments must equal the sale total."));
    }
    let change = ((tendered_sum - paid) * 100.0).round() / 100.0;

    let mut tx = state.pool.begin().await?;
    let receipt = crate::refs::next_sale_receipt(&mut tx, &state.pool).await?;
    let created_at = Utc::now().to_rfc3339();

    let result = sqlx::query(
        "INSERT INTO sales(receipt_number, user_id, branch_id, stock_location_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at)
         VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'completed', ?)",
    )
    .bind(&receipt)
    .bind(auth.user.id)
    .bind(branch_id)
    .bind(location_id)
    .bind(body.customer_name.as_deref())
    .bind(&auth.user.name)
    .bind(format!("{subtotal:.2}"))
    .bind(format!("{discount:.2}"))
    .bind(format!("{tax:.2}"))
    .bind(format!("{total:.2}"))
    .bind(format!("{change:.2}"))
    .bind(&created_at)
    .execute(&mut *tx)
    .await?;
    let sale_id = result.last_insert_rowid();

    for (product_id, name, sku, unit, qty, line_total, sell_location, variant_id) in &built {
        sqlx::query(
            "INSERT INTO sale_items(sale_id, product_id, product_variant_id, name, sku, unit_price, quantity, line_total)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(sale_id)
        .bind(product_id)
        .bind(variant_id)
        .bind(name)
        .bind(sku)
        .bind(format!("{unit:.2}"))
        .bind(qty)
        .bind(format!("{line_total:.2}"))
        .execute(&mut *tx)
        .await?;

        if let Some(vid) = variant_id {
            sqlx::query(
                "UPDATE inventory_balances
                 SET on_hand = on_hand - ?
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id = ?",
            )
            .bind(qty)
            .bind(product_id)
            .bind(sell_location)
            .bind(vid)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "UPDATE inventory_balances
                 SET on_hand = on_hand - ?
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(qty)
            .bind(product_id)
            .bind(sell_location)
            .execute(&mut *tx)
            .await?;
        }

        let after: i64 = if let Some(vid) = variant_id {
            sqlx::query_scalar(
                "SELECT on_hand FROM inventory_balances
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id = ?",
            )
            .bind(product_id)
            .bind(sell_location)
            .bind(vid)
            .fetch_one(&mut *tx)
            .await?
        } else {
            sqlx::query_scalar(
                "SELECT on_hand FROM inventory_balances
                 WHERE product_id = ? AND stock_location_id = ? AND product_variant_id IS NULL",
            )
            .bind(product_id)
            .bind(sell_location)
            .fetch_one(&mut *tx)
            .await?
        };

        sqlx::query(
            "INSERT INTO stock_movements(product_id, product_variant_id, stock_location_id, branch_id, user_id, kind, quantity, balance_after, document_type, document_id)
             VALUES(?, ?, ?, ?, ?, 'sale', ?, ?, 'sale', ?)",
        )
        .bind(product_id)
        .bind(variant_id)
        .bind(sell_location)
        .bind(branch_id)
        .bind(auth.user.id)
        .bind(-qty)
        .bind(after)
        .bind(sale_id)
        .execute(&mut *tx)
        .await?;

        let event_id = uuid::Uuid::new_v4().to_string();
        let payload = json!({
            "sale_id": sale_id,
            "product_id": product_id,
            "product_variant_id": variant_id,
            "quantity": qty,
            "receipt_number": receipt,
        });
        sqlx::query(
            "INSERT INTO outbox(event_id, event_type, payload_json) VALUES(?, 'sale.item', ?)",
        )
        .bind(&event_id)
        .bind(payload.to_string())
        .execute(&mut *tx)
        .await?;
    }

    for (method, amount, tendered) in &pay_rows {
        sqlx::query(
            "INSERT INTO sale_payments(sale_id, method, amount, tendered) VALUES(?, ?, ?, ?)",
        )
        .bind(sale_id)
        .bind(method)
        .bind(format!("{amount:.2}"))
        .bind(format!("{tendered:.2}"))
        .execute(&mut *tx)
        .await?;
    }

    let sale_event = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO outbox(event_id, event_type, payload_json) VALUES(?, 'sale.completed', ?)")
        .bind(&sale_event)
        .bind(
            json!({
                "sale_id": sale_id,
                "receipt_number": receipt,
                "total": format!("{total:.2}"),
            })
            .to_string(),
        )
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let row = sqlx::query_as::<_, SaleRow>(
        "SELECT id, receipt_number, user_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at
         FROM sales WHERE id = ?",
    )
    .bind(sale_id)
    .fetch_one(&state.pool)
    .await?;
    let response = json!({ "sale": sale_json(&state, &row, true).await? });

    if let Some(key) = idem {
        let _ = sqlx::query("INSERT OR IGNORE INTO idempotency_keys(key, response_json) VALUES(?, ?)")
            .bind(key)
            .bind(response.to_string())
            .execute(&state.pool)
            .await;
    }

    activity::log(
        &state,
        &auth,
        "sale.completed",
        format!(
            "{} completed sale {} for {}",
            auth.user.name, receipt, format!("{total:.2}")
        ),
        Some(json!({
            "receipt_number": receipt,
            "total": format!("{total:.2}"),
            "items": built.len(),
        })),
    )
    .await;

    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Debug, Deserialize)]
pub struct ReturnBody {
    #[serde(default)]
    pub items: Option<Vec<ReturnItemIn>>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub refund_method: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReturnItemIn {
    pub sale_item_id: i64,
    pub quantity: i64,
}

pub async fn create_return(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<ReturnBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    auth.require("history.return")?;
    let row = sqlx::query_as::<_, SaleRow>(
        "SELECT id, receipt_number, user_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at
         FROM sales WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Sale not found.".into()))?;

    let items_in = body.items.unwrap_or_default();
    if items_in.is_empty() {
        return Err(AppError::bad("Select at least one item to return."));
    }
    let method = body
        .refund_method
        .or(body.method)
        .unwrap_or_else(|| "cash".into())
        .trim()
        .to_lowercase();
    let location_id = db::store_location_id(&state.pool).await?;
    let branch_id = db::meta_get(&state.pool, "branch_id")
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let mut refund_total = 0.0;
    let mut return_lines = Vec::new();
    for item in &items_in {
        if item.quantity < 1 {
            return Err(AppError::bad("Return quantity must be at least 1."));
        }
        let line: Option<(i64, i64, String, String, i64, i64)> = sqlx::query_as(
            "SELECT si.id, si.product_id, si.name, si.unit_price, si.quantity,
                    COALESCE((SELECT SUM(sri.quantity) FROM sale_return_items sri
                              JOIN sale_returns sr ON sr.id = sri.return_id
                              WHERE sri.sale_item_id = si.id), 0)
             FROM sale_items si WHERE si.id = ? AND si.sale_id = ?",
        )
        .bind(item.sale_item_id)
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
        let Some((sale_item_id, product_id, name, unit_price, sold_qty, returned_qty)) = line else {
            return Err(AppError::bad("A return line is invalid."));
        };
        let returnable = sold_qty - returned_qty;
        if item.quantity > returnable {
            return Err(AppError::bad(format!("Cannot return more than sold for {name}.")));
        }
        let unit: f64 = unit_price.parse().unwrap_or(0.0);
        let line_total = ((unit * item.quantity as f64) * 100.0).round() / 100.0;
        refund_total += line_total;
        return_lines.push((sale_item_id, product_id, name, item.quantity, line_total));
    }

    let mut tx = state.pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO sale_returns(sale_id, user_id, reason, method, total) VALUES(?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(auth.user.id)
    .bind(body.reason.as_deref())
    .bind(&method)
    .bind(format!("{refund_total:.2}"))
    .execute(&mut *tx)
    .await?;
    let return_id = result.last_insert_rowid();
    let mut receipt_items = Vec::new();
    for (sale_item_id, product_id, name, qty, line_total) in &return_lines {
        sqlx::query(
            "INSERT INTO sale_return_items(return_id, sale_item_id, product_id, quantity) VALUES(?, ?, ?, ?)",
        )
        .bind(return_id)
        .bind(sale_item_id)
        .bind(product_id)
        .bind(qty)
        .execute(&mut *tx)
        .await?;
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
             VALUES(?, ?, ?, ?, 'return', ?, ?, 'sale_return', ?)",
        )
        .bind(product_id)
        .bind(location_id)
        .bind(branch_id)
        .bind(auth.user.id)
        .bind(qty)
        .bind(after)
        .bind(return_id)
        .execute(&mut *tx)
        .await?;
        receipt_items.push(json!({
            "name": name,
            "quantity": qty,
            "line_total": format!("{line_total:.2}"),
        }));
    }
    let event_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO outbox(event_id, event_type, payload_json) VALUES(?, 'sale.returned', ?)")
        .bind(&event_id)
        .bind(json!({ "sale_id": id, "return_id": return_id }).to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let updated = sqlx::query_as::<_, SaleRow>(
        "SELECT id, receipt_number, user_id, customer_name, cashier_name, subtotal, discount, tax, total, change_amount, status, created_at
         FROM sales WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    let mut sale = sale_json(&state, &updated, true).await?;
    // refresh returnable quantities
    if let Some(items) = sale.get_mut("items").and_then(|v| v.as_array_mut()) {
        for item in items {
            if let Some(item_id) = item.get("id").and_then(|v| v.as_i64()) {
                let returned: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(SUM(sri.quantity), 0) FROM sale_return_items sri
                     JOIN sale_returns sr ON sr.id = sri.return_id WHERE sri.sale_item_id = ?",
                )
                .bind(item_id)
                .fetch_one(&state.pool)
                .await
                .unwrap_or(0);
                let qty = item.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
                item["returned_quantity"] = json!(returned);
                item["returnable_quantity"] = json!((qty - returned).max(0));
            }
        }
        sale["sale_items"] = sale["items"].clone();
    }

    activity::log(
        &state,
        &auth,
        "sale.returned",
        format!(
            "{} returned {} on sale {} (refund {})",
            auth.user.name,
            receipt_items.len(),
            row.receipt_number,
            format!("{refund_total:.2}")
        ),
        Some(json!({
            "sale_id": id,
            "return_id": return_id,
            "receipt_number": row.receipt_number,
            "total": format!("{refund_total:.2}"),
            "method": method,
        })),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "sale": sale,
            "return": {
                "id": return_id,
                "receipt_number": format!("RET{}", row.receipt_number),
                "total": format!("{refund_total:.2}"),
                "method": method,
                "items": receipt_items,
            }
        })),
    ))
}

async fn sale_json(state: &AppState, row: &SaleRow, detail: bool) -> AppResult<Value> {
    let items: Vec<(i64, i64, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, product_id, name, sku, unit_price, quantity, line_total FROM sale_items WHERE sale_id = ?",
    )
    .bind(row.id)
    .fetch_all(&state.pool)
    .await?;
    let payments: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, method, amount, tendered FROM sale_payments WHERE sale_id = ?",
    )
    .bind(row.id)
    .fetch_all(&state.pool)
    .await?;
    let items_count: i64 = items.iter().map(|i| i.5).sum();
    let payment_methods: Vec<String> = payments.iter().map(|p| p.1.clone()).collect();
    let payment_values: Vec<Value> = payments
        .iter()
        .map(|(id, method, amount, tendered)| {
            json!({
                "id": id,
                "method": method,
                "amount": amount,
                "tendered": tendered,
            })
        })
        .collect();

    let mut value = json!({
        "id": row.id,
        "receipt_number": row.receipt_number,
        "user_id": row.user_id,
        "customer_name": row.customer_name.clone().unwrap_or_else(|| "Walk-in".into()),
        "cashier_name": row.cashier_name,
        "served_by": row.cashier_name,
        "subtotal": row.subtotal,
        "discount": row.discount,
        "tax": row.tax,
        "total": row.total,
        "credit": "0.00",
        "change": row.change_amount,
        "status": row.status,
        "created_at": row.created_at,
        "items_count": items_count,
        "payment_methods": payment_methods,
        "payments": payment_values,
    });
    if detail {
        let mut item_values = Vec::new();
        for (id, product_id, name, sku, unit_price, quantity, line_total) in items {
            let returned: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(sri.quantity), 0) FROM sale_return_items sri
                 JOIN sale_returns sr ON sr.id = sri.return_id WHERE sri.sale_item_id = ?",
            )
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
            item_values.push(json!({
                "id": id,
                "product_id": product_id,
                "name": name,
                "sku": sku,
                "unit_price": unit_price,
                "quantity": quantity,
                "line_total": line_total,
                "returned_quantity": returned,
                "returnable_quantity": (quantity - returned).max(0),
            }));
        }
        value["items"] = Value::Array(item_values);
        value["sale_items"] = value["items"].clone();
        value["sale_payments"] = value["payments"].clone();
    }
    Ok(value)
}

fn parse_money(raw: &str) -> AppResult<f64> {
    let cleaned = raw.trim().replace(',', "");
    cleaned
        .parse::<f64>()
        .map_err(|_| AppError::bad("Amount must be a number."))
}
