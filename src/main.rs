use clap::Parser;
use synkmart_host::{config::Config, run_server};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()))
        .init();

    let config = Config::parse();
    if config.version {
        println!("synkmart-host {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    #[cfg(windows)]
    {
        if !config.console {
            // When started by the Service Control Manager, run as a service.
            // Fall through to console if that fails (e.g. interactive debug).
            match synkmart_host::service::windows::run_as_service() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "service dispatcher unavailable; running in console mode");
                }
            }
        }
    }

    run_server(config).await
}
