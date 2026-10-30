pub mod activity;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod money;
pub mod outbox;
pub mod refs;
pub mod routes;
pub mod service;
pub mod state;

use config::Config;
use state::AppState;

pub async fn run_server(config: Config) -> anyhow::Result<()> {
    let db_path = config.database_path();
    tracing::info!(path = %db_path.display(), "opening store database");
    let pool = db::connect(&db_path).await?;
    let bind = config.bind.clone();
    let state = AppState::new(pool, config);
    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "synkmart-host listening");
    axum::serve(listener, app).await?;
    Ok(())
}
