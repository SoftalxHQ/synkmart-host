use crate::{
    auth::hash_password,
    db,
    error::{AppError, AppResult},
    state::AppState,
};
use axum::{Json, extract::State};
use chrono::{Duration, Utc};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;

pub async fn status(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let bootstrapped = is_bootstrapped(&state.pool).await?;
    let merchant_name = db::meta_get(&state.pool, "merchant_name").await?;
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await?;
    let product_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({
        "bootstrapped": bootstrapped,
        "merchant_name": merchant_name,
        "user_count": user_count,
        "product_count": product_count,
        "bind": state.config.bind,
        "cloud_url": state.config.cloud_url,
    })))
}

#[derive(Debug, Deserialize)]
pub struct LocalBootstrap {
    pub business_name: String,
    pub admin_name: String,
    pub admin_email: String,
    pub admin_password: String,
    #[serde(default)]
    pub branch_name: Option<String>,
    /// Ignored — POS always sells from store. Kept for older clients.
    #[serde(default)]
    pub sell_from: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

pub async fn local_bootstrap(
    State(state): State<AppState>,
    Json(body): Json<LocalBootstrap>,
) -> AppResult<Json<Value>> {
    if is_bootstrapped(&state.pool).await? {
        return Err(AppError::Conflict("This host is already set up.".into()));
    }
    let name = body.business_name.trim();
    let admin_name = body.admin_name.trim();
    let email = body.admin_email.trim().to_lowercase();
    let password = body.admin_password;
    let branch_name = body
        .branch_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Branch 1");
    let _ = body.sell_from; // POS always sells from store; warehouse stock must be transferred.
    if name.is_empty() || admin_name.is_empty() || email.is_empty() || password.len() < 6 {
        return Err(AppError::bad(
            "Business name, admin name, email, and a password of at least 6 characters are required.",
        ));
    }

    let digest = hash_password(&password)?;
    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO users(id, name, email, role, title, status, password_digest, permissions_json)
         VALUES(1, ?, ?, 'admin', 'Owner', 'active', ?, '[]')",
    )
    .bind(admin_name)
    .bind(&email)
    .bind(&digest)
    .execute(&mut *tx)
    .await?;

    // Local host: warehouse (id 1) + shop floor (id 2). POS sells from store only.
    let stock_location_id = "2";
    let stock_location_name = "Shop floor";
    let sell_from = "store";

    let device_id = uuid::Uuid::new_v4().to_string();
    let merchant_code = db::generate_merchant_code();
    let currency = body.currency.as_deref().unwrap_or("NGN");
    for (k, v) in [
        ("bootstrapped", "1"),
        ("merchant_id", "0"),
        ("merchant_name", name),
        ("merchant_slug", "local"),
        ("merchant_code", merchant_code.as_str()),
        ("currency", currency),
        ("branch_id", "1"),
        ("branch_name", branch_name),
        ("stock_location_id", stock_location_id),
        ("stock_location_name", stock_location_name),
        ("stock_location_kind", sell_from),
        ("warehouse_location_id", "1"),
        ("store_location_id", "2"),
        ("device_id", device_id.as_str()),
        ("enable_tax", "0"),
        ("tax_rate", "0"),
    ] {
        sqlx::query(
            "INSERT INTO store_meta(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(k)
        .bind(v)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO branches(id, name, slug, city, status) VALUES(1, ?, 'main', '', 'active')
         ON CONFLICT(id) DO UPDATE SET name = excluded.name",
    )
    .bind(branch_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO stock_locations(id, branch_id, name, kind, slug, status) VALUES
         (1, 1, 'Warehouse', 'warehouse', 'warehouse', 'active'),
         (2, 1, 'Shop floor', 'store', 'shop-floor', 'active')
         ON CONFLICT(id) DO NOTHING",
    )
    .execute(&mut *tx)
    .await?;

    let code = issue_pairing_code(&mut tx).await?;
    tx.commit().await?;

    Ok(Json(json!({
        "ok": true,
        "merchant_name": name,
        "merchant_code": merchant_code,
        "branch_name": branch_name,
        "stock_location_name": stock_location_name,
        "pairing_code": code,
        "urls": lan_urls(&state.config.bind),
        "bind": state.config.bind,
    })))
}

#[derive(Debug, Deserialize)]
pub struct CloudBootstrap {
    pub merchant_code: String,
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub cloud_url: Option<String>,
}

pub async fn cloud_bootstrap(
    State(state): State<AppState>,
    Json(body): Json<CloudBootstrap>,
) -> AppResult<Json<Value>> {
    if is_bootstrapped(&state.pool).await? {
        return Err(AppError::Conflict("This host is already set up.".into()));
    }

    let cloud = body
        .cloud_url
        .as_deref()
        .unwrap_or(state.config.cloud_url.as_str())
        .trim_end_matches('/');

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Other(e.into()))?;

    let session_res = client
        .post(format!("{cloud}/api/v1/auth/session"))
        .json(&json!({
            "email": body.email.trim().to_lowercase(),
            "password": body.password,
            "merchant_code": body.merchant_code.trim().to_uppercase(),
        }))
        .send()
        .await
        .map_err(|e| AppError::bad(format!("Could not reach cloud: {e}")))?;

    if !session_res.status().is_success() {
        let err = session_res
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "Cloud sign-in failed.".into());
        return Err(AppError::unauthorized(err));
    }
    let session: Value = session_res
        .json()
        .await
        .map_err(|e| AppError::bad(format!("Bad cloud session response: {e}")))?;
    let token = session
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| AppError::bad("Cloud session missing token."))?;

    let seed_res = client
        .get(format!("{cloud}/api/v1/host/seed"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| AppError::bad(format!("Could not download seed: {e}")))?;

    if !seed_res.status().is_success() {
        let err = seed_res
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| "Cloud seed export failed.".into());
        return Err(AppError::bad(err));
    }
    let seed: Value = seed_res
        .json()
        .await
        .map_err(|e| AppError::bad(format!("Bad seed response: {e}")))?;

    apply_seed(&state.pool, &seed).await?;

    let mut tx = state.pool.begin().await?;
    let code = issue_pairing_code(&mut tx).await?;
    tx.commit().await?;

    let lan = lan_urls(&state.config.bind);
    let device_id = db::meta_get(&state.pool, "device_id")
        .await?
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let branch_name = seed
        .get("branch")
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Store host");
    let merchant_code = seed
        .get("merchant")
        .and_then(|m| m.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Register device on cloud and email branch credentials / welcome to the admin.
    let mut email_sent = false;
    let mut welcome_email: Option<String> = None;
    match client
        .post(format!("{cloud}/api/v1/host/ready"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&json!({
            "device_id": device_id,
            "name": branch_name,
            "host_urls": lan,
            "send_email": true,
        }))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => {
            if let Ok(body) = res.json::<Value>().await {
                email_sent = body.get("email_sent").and_then(|v| v.as_bool()).unwrap_or(false);
                welcome_email = body
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(secret) = body.get("shared_secret").and_then(|v| v.as_str()) {
                    let _ = db::meta_set(&state.pool, "cloud_shared_secret", secret).await;
                }
                let _ = db::meta_set(&state.pool, "cloud_registered", "1").await;
            }
        }
        Ok(res) => {
            tracing::warn!(status = %res.status(), "cloud host/ready failed; seed still applied");
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not notify cloud host/ready; seed still applied");
        }
    }

    Ok(Json(json!({
        "ok": true,
        "merchant_name": seed.get("merchant").and_then(|m| m.get("name")).cloned(),
        "merchant_code": merchant_code,
        "branch_name": branch_name,
        "pairing_code": code,
        "bind": state.config.bind,
        "urls": lan,
        "users": seed.get("users").and_then(|u| u.as_array()).map(|a| a.len()).unwrap_or(0),
        "products": seed.get("products").and_then(|u| u.as_array()).map(|a| a.len()).unwrap_or(0),
        "email_sent": email_sent,
        "welcome_email": welcome_email,
    })))
}

pub async fn pairing_info(State(state): State<AppState>) -> AppResult<Json<Value>> {
    if !is_bootstrapped(&state.pool).await? {
        return Err(AppError::bad("Set up this host before pairing clients."));
    }
    let mut tx = state.pool.begin().await?;
    let code = issue_pairing_code(&mut tx).await?;
    tx.commit().await?;
    let lan_urls = lan_urls(&state.config.bind);
    Ok(Json(json!({
        "pairing_code": code,
        "bind": state.config.bind,
        "urls": lan_urls,
        "expires_in_seconds": 600,
    })))
}

#[derive(Debug, Deserialize)]
pub struct PairBody {
    pub code: String,
    #[serde(default)]
    pub host_url: Option<String>,
}

pub async fn pair_client(
    State(state): State<AppState>,
    Json(body): Json<PairBody>,
) -> AppResult<Json<Value>> {
    let code = body.code.trim().to_uppercase();
    let row: Option<(String,)> =
        sqlx::query_as("SELECT code FROM pairing WHERE code = ? AND expires_at > datetime('now')")
            .bind(&code)
            .fetch_optional(&state.pool)
            .await?;
    if row.is_none() {
        return Err(AppError::unauthorized("Pairing code is invalid or expired."));
    }
    let merchant_name = db::meta_get(&state.pool, "merchant_name")
        .await?
        .unwrap_or_else(|| "SynkMart".into());
    let urls = lan_urls(&state.config.bind);
    let host_url = body
        .host_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| {
            urls.first()
                .cloned()
                .unwrap_or_else(|| format!("http://{}", state.config.bind))
        });

    Ok(Json(json!({
        "ok": true,
        "host_url": host_url,
        "merchant_name": merchant_name,
        "urls": urls,
    })))
}

async fn is_bootstrapped(pool: &SqlitePool) -> AppResult<bool> {
    Ok(db::meta_get(pool, "bootstrapped").await?.as_deref() == Some("1"))
}

async fn issue_pairing_code(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> AppResult<String> {
    let code: String = (0..6)
        .map(|_| {
            const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            let idx = rand::rng().random_range(0..CHARS.len());
            CHARS[idx] as char
        })
        .collect();
    let expires = (Utc::now() + Duration::minutes(10))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    sqlx::query("DELETE FROM pairing").execute(&mut **tx).await?;
    sqlx::query("INSERT INTO pairing(code, expires_at) VALUES(?, ?)")
        .bind(&code)
        .bind(&expires)
        .execute(&mut **tx)
        .await?;
    Ok(code)
}

fn lan_urls(bind: &str) -> Vec<String> {
    let port = bind.rsplit(':').next().unwrap_or("8787");
    let mut urls = vec![format!("http://127.0.0.1:{port}")];
    if let Ok(ip) = detect_lan_ip() {
        urls.push(format!("http://{ip}:{port}"));
    }
    urls
}

fn detect_lan_ip() -> Result<String, std::io::Error> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let _ = socket.connect("8.8.8.8:80");
    Ok(socket.local_addr()?.ip().to_string())
}

async fn apply_seed(pool: &SqlitePool, seed: &Value) -> AppResult<()> {
    let merchant = seed
        .get("merchant")
        .ok_or_else(|| AppError::bad("Seed missing merchant."))?;
    let mut tx = pool.begin().await?;

    let mid = merchant.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = merchant
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Store");
    let slug = merchant.get("slug").and_then(|v| v.as_str()).unwrap_or("store");
    let code = merchant.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let currency = merchant
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("NGN");
    let branch_id = seed
        .get("branch")
        .and_then(|b| b.get("id"))
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    let location_id = seed
        .get("stock_location")
        .and_then(|b| b.get("id"))
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    let device_id = uuid::Uuid::new_v4().to_string();
    let tax_flag = if merchant.get("enable_tax").and_then(|v| v.as_bool()).unwrap_or(false) {
        "1"
    } else {
        "0"
    };
    let tax_rate = merchant
        .get("tax_rate")
        .map(|v| v.to_string().trim_matches('"').to_string())
        .unwrap_or_else(|| "0".into());

    let branch_name = seed
        .get("branch")
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Branch 1");
    let location_name = seed
        .get("stock_location")
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Shop floor");
    let location_kind = seed
        .get("stock_location")
        .and_then(|b| b.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("store");

    sqlx::query(
        "INSERT INTO branches(id, cloud_id, name, slug, city, status) VALUES(?, ?, ?, 'main', '', 'active')
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, cloud_id = excluded.cloud_id",
    )
    .bind(branch_id)
    .bind(branch_id)
    .bind(branch_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO stock_locations(id, cloud_id, branch_id, name, kind, slug, status)
         VALUES(?, ?, ?, ?, ?, 'main', 'active')
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, kind = excluded.kind, branch_id = excluded.branch_id",
    )
    .bind(location_id)
    .bind(location_id)
    .bind(branch_id)
    .bind(location_name)
    .bind(location_kind)
    .execute(&mut *tx)
    .await?;

    for (k, v) in [
        ("bootstrapped", "1".to_string()),
        ("merchant_id", mid.to_string()),
        ("merchant_name", name.to_string()),
        ("merchant_slug", slug.to_string()),
        ("merchant_code", code.to_string()),
        ("currency", currency.to_string()),
        ("branch_id", branch_id.to_string()),
        ("branch_name", branch_name.to_string()),
        ("stock_location_id", location_id.to_string()),
        ("stock_location_name", location_name.to_string()),
        ("stock_location_kind", location_kind.to_string()),
        ("device_id", device_id),
        ("enable_tax", tax_flag.to_string()),
        ("tax_rate", tax_rate),
        ("last_seed_at", Utc::now().to_rfc3339()),
    ] {
        sqlx::query(
            "INSERT INTO store_meta(key, value) VALUES(?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(k)
        .bind(v)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(users) = seed.get("users").and_then(|v| v.as_array()) {
        for user in users {
            let id = user.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let permissions = user
                .get("permissions")
                .cloned()
                .unwrap_or(Value::Array(vec![]));
            sqlx::query(
                "INSERT INTO users(id, cloud_user_id, name, email, role, title, status, password_digest, permissions_json)
                 VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                   name=excluded.name, email=excluded.email, role=excluded.role,
                   title=excluded.title, status=excluded.status,
                   password_digest=excluded.password_digest,
                   permissions_json=excluded.permissions_json",
            )
            .bind(id)
            .bind(id)
            .bind(user.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            .bind(
                user.get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase(),
            )
            .bind(user.get("role").and_then(|v| v.as_str()).unwrap_or("cashier"))
            .bind(user.get("title").and_then(|v| v.as_str()))
            .bind(user.get("status").and_then(|v| v.as_str()).unwrap_or("active"))
            .bind(
                user.get("password_digest")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
            .bind(permissions.to_string())
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(categories) = seed.get("categories").and_then(|v| v.as_array()) {
        for cat in categories {
            let id = cat.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            sqlx::query(
                "INSERT INTO categories(id, cloud_id, name) VALUES(?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name",
            )
            .bind(id)
            .bind(id)
            .bind(cat.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(products) = seed.get("products").and_then(|v| v.as_array()) {
        for product in products {
            let id = product.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            sqlx::query(
                "INSERT INTO products(id, cloud_id, category_id, name, sku, barcode, description, purchase_price, selling_price, reorder_level, featured)
                 VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                   category_id=excluded.category_id, name=excluded.name, sku=excluded.sku,
                   barcode=excluded.barcode, description=excluded.description,
                   purchase_price=excluded.purchase_price, selling_price=excluded.selling_price,
                   reorder_level=excluded.reorder_level, featured=excluded.featured",
            )
            .bind(id)
            .bind(id)
            .bind(product.get("category_id").and_then(|v| v.as_i64()))
            .bind(product.get("name").and_then(|v| v.as_str()).unwrap_or(""))
            .bind(product.get("sku").and_then(|v| v.as_str()).unwrap_or(""))
            .bind(product.get("barcode").and_then(|v| v.as_str()))
            .bind(product.get("description").and_then(|v| v.as_str()))
            .bind(money_str(product.get("purchase_price")))
            .bind(money_str(product.get("selling_price")))
            .bind(product.get("reorder_level").and_then(|v| v.as_i64()))
            .bind(i64::from(
                product.get("featured").and_then(|v| v.as_bool()).unwrap_or(false),
            ))
            .execute(&mut *tx)
            .await?;

            if let Some(balances) = product.get("balances").and_then(|v| v.as_array()) {
                for bal in balances {
                    sqlx::query(
                        "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
                         VALUES(?, ?, ?, ?, ?)
                         ON CONFLICT(product_id, stock_location_id) DO UPDATE SET
                           on_hand=excluded.on_hand, reserved=excluded.reserved, branch_id=excluded.branch_id",
                    )
                    .bind(id)
                    .bind(bal.get("branch_id").and_then(|v| v.as_i64()).unwrap_or(branch_id))
                    .bind(
                        bal.get("stock_location_id")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(location_id),
                    )
                    .bind(bal.get("on_hand").and_then(|v| v.as_i64()).unwrap_or(0))
                    .bind(bal.get("reserved").and_then(|v| v.as_i64()).unwrap_or(0))
                    .execute(&mut *tx)
                    .await?;
                }
            } else if let Some(qty) = product.get("stock_quantity").and_then(|v| v.as_i64()) {
                sqlx::query(
                    "INSERT INTO inventory_balances(product_id, branch_id, stock_location_id, on_hand, reserved)
                     VALUES(?, ?, ?, ?, 0)
                     ON CONFLICT(product_id, stock_location_id) DO UPDATE SET on_hand=excluded.on_hand",
                )
                .bind(id)
                .bind(branch_id)
                .bind(location_id)
                .bind(qty)
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok(())
}

fn money_str(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "0".into(),
    }
}
