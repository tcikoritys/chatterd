use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use tracing::warn;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Where daemon state should live (dbs, caches, etc).
    pub state_dir: Option<PathBuf>,
    /// Placeholder for future RPC listen addr.
    pub rpc_listen: Option<String>,
}

impl Config {
    pub fn load() -> Self {
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
