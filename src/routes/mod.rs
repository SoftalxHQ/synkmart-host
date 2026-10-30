mod activity_logs;
mod auth;
mod branches;
mod categories;
mod customers;
mod health;
mod merchant;
mod products;
mod promos;
mod purchases;
mod register;
mod sales;
mod setup;
mod staff;
mod stock;
mod suppliers;
mod transfers;

use crate::state::AppState;
use axum::{
    Router,
    routing::{get, patch, post},
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health::health))
        .route("/api/v1/setup/status", get(setup::status))
        .route("/api/v1/setup/local", post(setup::local_bootstrap))
        .route("/api/v1/setup/cloud", post(setup::cloud_bootstrap))
        .route("/api/v1/setup/pairing", get(setup::pairing_info))
        .route("/api/v1/setup/pair", post(setup::pair_client))
        .route("/api/v1/auth/session", post(auth::sign_in).delete(auth::sign_out))
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/auth/lock", post(auth::lock))
        .route("/api/v1/merchant", get(merchant::show).patch(merchant::update))
        .route("/api/v1/profile", patch(merchant::profile_update))
        .route(
            "/api/v1/categories",
            get(categories::index).post(categories::create),
        )
        .route(
            "/api/v1/categories/{id}",
            patch(categories::update).delete(categories::destroy),
        )
        .route(
            "/api/v1/products",
            get(products::index).post(products::create),
        )
        .route("/api/v1/products/import", post(products::import))
        .route("/api/v1/products/alert_counts", get(products::alert_counts))
        .route(
            "/api/v1/products/{id}",
            get(products::show)
                .patch(products::update)
                .delete(products::destroy),
        )
        .route("/api/v1/products/{id}/restock", post(products::restock))
        .route(
            "/api/v1/register_sessions",
            get(register::index).post(register::create),
        )
        .route("/api/v1/register_sessions/{id}/close", post(register::close))
        .route("/api/v1/sales", get(sales::index).post(sales::create))
        .route("/api/v1/sales/{id}", get(sales::show))
        .route("/api/v1/sales/{id}/returns", post(sales::create_return))
        .route(
            "/api/v1/branches",
            get(branches::index).post(branches::create),
        )
        .route(
            "/api/v1/branches/{id}",
            get(branches::show)
                .patch(branches::update)
                .delete(branches::destroy),
        )
        .route("/api/v1/branches/{id}/disable", post(branches::disable))
        .route("/api/v1/branches/{id}/restore", post(branches::restore))
        .route(
            "/api/v1/branches/{branch_id}/stock_locations",
            get(branches::locations_index).post(branches::locations_create),
        )
        .route(
            "/api/v1/branches/{branch_id}/stock_locations/{id}",
            patch(branches::locations_update).delete(branches::locations_destroy),
        )
        .route(
            "/api/v1/branches/{branch_id}/stock_locations/{id}/disable",
            post(branches::locations_disable),
        )
        .route(
            "/api/v1/branches/{branch_id}/stock_locations/{id}/restore",
            post(branches::locations_restore),
        )
        .route("/api/v1/stock_locations", get(branches::merchant_locations))
        .route(
            "/api/v1/customers",
            get(customers::index).post(customers::create),
        )
        .route(
            "/api/v1/customers/{id}",
            get(customers::show)
                .patch(customers::update)
                .delete(customers::destroy),
        )
        .route("/api/v1/customers/{id}/repayments", post(customers::repay))
        .route("/api/v1/customers/{id}/transactions", get(customers::transactions))
        .route(
            "/api/v1/customers/{id}/transactions/{txn_id}",
            get(customers::transaction_show),
        )
        .route(
            "/api/v1/suppliers",
            get(suppliers::index).post(suppliers::create),
        )
        .route(
            "/api/v1/suppliers/{id}",
            get(suppliers::show)
                .patch(suppliers::update)
                .delete(suppliers::destroy),
        )
        .route("/api/v1/suppliers/{id}/suspend", post(suppliers::suspend))
        .route("/api/v1/suppliers/{id}/restore", post(suppliers::restore))
        .route("/api/v1/suppliers/{id}/ledger", get(suppliers::ledger))
        .route("/api/v1/suppliers/{id}/payments", post(suppliers::create_payment))
        .route(
            "/api/v1/suppliers/{id}/payments/{payment_id}/notify",
            post(suppliers::notify_payment),
        )
        .route(
            "/api/v1/purchases",
            get(purchases::index).post(purchases::create),
        )
        .route("/api/v1/purchases/open_count", get(purchases::open_count))
        .route("/api/v1/purchases/{id}", get(purchases::show))
        .route("/api/v1/purchases/{id}/submit", post(purchases::submit))
        .route("/api/v1/purchases/{id}/issue", post(purchases::issue))
        .route("/api/v1/purchases/{id}/receive", post(purchases::receive))
        .route("/api/v1/purchases/{id}/notify", post(purchases::notify))
        .route("/api/v1/purchases/{id}/close", post(purchases::close))
        .route(
            "/api/v1/purchases/{id}/receipt_images",
            post(purchases::receipt_images),
        )
        .route(
            "/api/v1/purchase_images/{id}",
            get(purchases::show_receipt_image),
        )
        .route(
            "/api/v1/stock_transfers",
            get(transfers::index).post(transfers::create),
        )
        .route("/api/v1/stock_transfers/open_count", get(transfers::open_count))
        .route(
            "/api/v1/stock_transfers/{id}",
            get(transfers::show).patch(transfers::update),
        )
        .route(
            "/api/v1/stock_adjustments",
            get(stock::adjustments_index).post(stock::adjustments_create),
        )
        .route(
            "/api/v1/stock_adjustments/{id}/approve",
            post(stock::adjustments_approve),
        )
        .route(
            "/api/v1/stock_adjustments/{id}/reject",
            post(stock::adjustments_reject),
        )
        .route("/api/v1/stock_movements", get(stock::movements_index))
        .route("/api/v1/stock_reports/valuation", get(stock::report_valuation))
        .route("/api/v1/stock_reports/in_transit", get(stock::report_in_transit))
        .route(
            "/api/v1/stock_reports/discrepancies",
            get(stock::report_discrepancies),
        )
        .route("/api/v1/staff", get(staff::index).post(staff::create))
        .route(
            "/api/v1/staff/{id}",
            patch(staff::update).delete(staff::destroy),
        )
        .route("/api/v1/staff/{id}/suspend", post(staff::suspend))
        .route("/api/v1/staff/{id}/restore", post(staff::restore))
        .route("/api/v1/activity_logs", get(activity_logs::index))
        .route("/api/v1/promotions", get(promos::promotions_index))
        .route("/api/v1/promotions/active", get(promos::promotions_active))
        .route("/api/v1/promo_codes", get(promos::promo_codes_index))
        .route("/api/v1/promo_codes/validate", post(promos::validate_code))
        .route("/api/v1/promotional_banners", get(promos::banners_index))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
