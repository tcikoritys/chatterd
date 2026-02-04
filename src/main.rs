#![recursion_limit = "256"]

use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tokio::signal;
use tokio::sync::broadcast;
use tokio::time::interval;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

mod config;
mod matrix;
mod rpc;
mod state;

use crate::config::Config;
use crate::matrix::MatrixRuntime;
use crate::state::State;

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to init tracing subscriber");

    let config = Config::load();
    info!(?config, "chatterd starting");

    let state_dir = config
        .state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("./state"));
    if let Err(err) = fs::create_dir_all(&state_dir) {
        warn!("failed to create state dir {}: {}", state_dir.display(), err);
    }

    let state = std::sync::Arc::new(tokio::sync::Mutex::new(State::load(state_dir)));
    let runtime = std::sync::Arc::new(tokio::sync::Mutex::new(MatrixRuntime::new()));

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let rpc_addr = config
        .rpc_listen
        .clone()
        .unwrap_or_else(|| "127.0.0.1:9388".to_string());
    let rpc_shutdown = shutdown_tx.subscribe();
    let rpc_state = state.clone();
    let rpc_runtime = runtime.clone();
    tokio::spawn(async move {
        if let Err(err) =
            rpc::run_rpc_server(&rpc_addr, rpc_state, rpc_runtime, rpc_shutdown).await
        {
            warn!("rpc server failed: {}", err);
        }
    });

    // Placeholder loop; will be replaced by Matrix session + richer RPC methods.
    let mut tick = interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("shutdown requested");
                let _ = shutdown_tx.send(());
                break;
            }
            _ = tick.tick() => {
                info!("daemon heartbeat");
            }
        }
    }

    info!("chatterd stopped");
}
