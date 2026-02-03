use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tokio::signal;
use tokio::time::interval;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[derive(Debug, Deserialize)]
struct Config {
    /// Where daemon state should live (dbs, caches, etc).
    state_dir: Option<PathBuf>,
    /// Placeholder for future RPC listen addr.
    rpc_listen: Option<String>,
}

impl Config {
    fn load() -> Self {
        let path = std::env::var("CHATTERD_CONFIG").unwrap_or_else(|_| "chatterd.toml".into());
        match fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str::<Config>(&contents) {
                Ok(cfg) => cfg,
                Err(err) => {
                    warn!("invalid config at {}: {}", path, err);
                    Config::default()
                }
            },
            Err(err) => {
                warn!("config not found at {}: {}", path, err);
                Config::default()
            }
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_dir: Some(PathBuf::from("./state")),
            rpc_listen: Some("127.0.0.1:9388".to_string()),
        }
    }
}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to init tracing subscriber");

    let config = Config::load();
    info!(?config, "chatterd starting");

    // Placeholder loop; will be replaced by Matrix session + JSON-RPC server.
    let mut tick = interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("shutdown requested");
                break;
            }
            _ = tick.tick() => {
                info!("daemon heartbeat");
            }
        }
    }

    info!("chatterd stopped");
}
