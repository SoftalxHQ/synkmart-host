use crate::{db, state::AppState};
use axum::{Json, extract::State};
use serde_json::{Value, json};

pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let store_id = db::meta_get(&state.pool, "store_id")
        .await
        .ok()
        .flatten();
    let merchant_name = db::meta_get(&state.pool, "merchant_name")
        .await
        .ok()
        .flatten();
    let bootstrapped = db::meta_get(&state.pool, "bootstrapped")
        .await
        .ok()
        .flatten()
        .as_deref()
        == Some("1");
    let bind = state.config.bind.clone();

    let last_seed_at = db::meta_get(&state.pool, "last_seed_at")
        .await
        .ok()
        .flatten();
    let product_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM products")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);

    Json(json!({
        "ok": true,
        "service": "synkmart-host",
        "version": env!("CARGO_PKG_VERSION"),
        "bind": bind,
        "bootstrapped": bootstrapped,
        "store_id": store_id,
        "merchant_name": merchant_name,
        "last_seed_at": last_seed_at,
        "product_count": product_count,
        "host_mode": true,
        "sync_status": if bootstrapped { "local" } else { "setup_required" },
    }))
}
