use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Clone, Parser)]
#[command(name = "synkmart-host", about = "SynkMart store host service")]
pub struct Config {
    /// HTTP bind address
    #[arg(long, env = "SYNKMART_HOST_BIND", default_value = "0.0.0.0:8787")]
    pub bind: String,

    /// SQLite database path
    #[arg(long, env = "SYNKMART_HOST_DB")]
    pub database: Option<PathBuf>,

    /// Cloud Rails API base URL used for bootstrap
    #[arg(long, env = "SYNKMART_CLOUD_URL", default_value = "http://127.0.0.1:3000")]
    pub cloud_url: String,

    /// Run as a foreground console process (default)
    #[arg(long)]
    pub console: bool,

    /// Print version and exit
    #[arg(long)]
    pub version: bool,
}

impl Config {
    pub fn database_path(&self) -> PathBuf {
        if let Some(path) = &self.database {
            return path.clone();
        }
        default_data_dir().join("store.db")
    }
}

pub fn default_data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".synkmart").join("host");
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("SynkMart").join("host");
    }
    PathBuf::from(".").join("data")
}
