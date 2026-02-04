use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::broadcast;
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

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LoginMethod {
    Password,
    Sso,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccountStatus {
    NeedsLogin,
    LoginInProgress,
    Ready,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct Account {
    id: String,
    homeserver: String,
    login_method: LoginMethod,
    status: AccountStatus,
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct PersistedState {
    next_id: u64,
    accounts: Vec<Account>,
}

#[derive(Debug)]
struct State {
    next_id: u64,
    accounts: Vec<Account>,
    state_dir: PathBuf,
}

impl State {
    fn load(state_dir: PathBuf) -> Self {
        let path = state_dir.join("accounts.json");
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Ok(persisted) = serde_json::from_str::<PersistedState>(&contents) {
                return Self {
                    next_id: persisted.next_id,
                    accounts: persisted.accounts,
                    state_dir,
                };
            }
        }

        Self {
            next_id: 1,
            accounts: Vec::new(),
            state_dir,
        }
    }

    fn save(&self) -> Result<(), RpcError> {
        let path = self.state_dir.join("accounts.json");
        let persisted = PersistedState {
            next_id: self.next_id,
            accounts: self.accounts.clone(),
        };
        let contents = serde_json::to_string_pretty(&persisted).map_err(|err| RpcError {
            code: -32603,
            message: format!("persist failed: {err}"),
        })?;
        fs::write(&path, contents).map_err(|err| RpcError {
            code: -32603,
            message: format!("persist failed: {err}"),
        })?;
        Ok(())
    }
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

    let state_dir = config
        .state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("./state"));
    if let Err(err) = fs::create_dir_all(&state_dir) {
        warn!("failed to create state dir {}: {}", state_dir.display(), err);
    }

    let state = std::sync::Arc::new(tokio::sync::Mutex::new(State::load(state_dir)));

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let rpc_addr = config
        .rpc_listen
        .clone()
        .unwrap_or_else(|| "127.0.0.1:9388".to_string());
    let rpc_shutdown = shutdown_tx.subscribe();
    let rpc_state = state.clone();
    tokio::spawn(async move {
        if let Err(err) = run_rpc_server(&rpc_addr, rpc_state, rpc_shutdown).await {
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

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    method: Option<String>,
    params: Option<serde_json::Value>,
    id: Option<serde_json::Value>,
}

#[derive(Debug, serde::Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, serde::Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
    id: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AccountAddParams {
    homeserver: String,
    login_method: Option<LoginMethod>,
}

#[derive(Debug, Deserialize)]
struct LoginStartParams {
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct LoginCompleteParams {
    account_id: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct MatrixHomeserverParams {
    homeserver: String,
}

#[derive(Debug)]
struct FetchResult {
    value: serde_json::Value,
    parse_warning: Option<String>,
}

async fn run_rpc_server(
    addr: &str,
    state: std::sync::Arc<tokio::sync::Mutex<State>>,
    mut shutdown: broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("json-rpc listening on {}", addr);

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("json-rpc shutdown requested");
                break;
            }
            accept = listener.accept() => {
                let (stream, peer) = accept?;
                info!("json-rpc client connected: {}", peer);
                let client_state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_client(stream, client_state).await {
                        warn!("json-rpc client error: {}", err);
                    }
                });
            }
        }
    }

    Ok(())
}

async fn handle_client(
    stream: TcpStream,
    state: std::sync::Arc<tokio::sync::Mutex<State>>,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let request: RpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(_) => {
                let resp = RpcResponse {
                    jsonrpc: "2.0",
                    result: None,
                    error: Some(RpcError {
                        code: -32700,
                        message: "parse error".to_string(),
                    }),
                    id: serde_json::Value::Null,
                };
                let payload = serde_json::to_string(&resp)?;
                write_half.write_all(payload.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
                continue;
            }
        };

        let response = match handle_request(&request, &state).await {
            Ok(Some(result)) => Some(RpcResponse {
                jsonrpc: "2.0",
                result: Some(result),
                error: None,
                id: request.id.clone().unwrap_or(serde_json::Value::Null),
            }),
            Ok(None) => None,
            Err(err) => Some(RpcResponse {
                jsonrpc: "2.0",
                result: None,
                error: Some(err),
                id: request.id.clone().unwrap_or(serde_json::Value::Null),
            }),
        };

        if let Some(resp) = response {
            let should_respond = request.id.is_some()
                || resp
                    .error
                    .as_ref()
                    .is_some_and(|err| err.code == -32600);
            if should_respond {
                let payload = serde_json::to_string(&resp)?;
                write_half.write_all(payload.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
            }
        }
    }

    Ok(())
}

async fn handle_request(
    request: &RpcRequest,
    state: &std::sync::Arc<tokio::sync::Mutex<State>>,
) -> Result<Option<serde_json::Value>, RpcError> {
    if request.jsonrpc.as_deref() != Some("2.0") {
        return Err(RpcError {
            code: -32600,
            message: "invalid request".to_string(),
        });
    }

    let method = request.method.as_deref().ok_or_else(|| RpcError {
        code: -32600,
        message: "invalid request".to_string(),
    })?;

    match method {
        "rpc.ping" => Ok(Some(serde_json::Value::String("pong".to_string()))),
        "rpc.version" => Ok(Some(serde_json::Value::String(env!(
            "CARGO_PKG_VERSION"
        )
        .to_string()))),
        "account.list" => {
            let state = state.lock().await;
            Ok(Some(serde_json::to_value(&state.accounts).map_err(|err| RpcError {
                code: -32603,
                message: format!("encode failed: {err}"),
            })?))
        }
        "account.add" => {
            let params = parse_params::<AccountAddParams>(request.params.as_ref())?;
            let mut state = state.lock().await;
            let id = format!("acct-{}", state.next_id);
            state.next_id += 1;
            let login_method = params.login_method.unwrap_or(LoginMethod::Sso);
            let account = Account {
                id: id.clone(),
                homeserver: params.homeserver,
                login_method,
                status: AccountStatus::NeedsLogin,
            };
            state.accounts.push(account.clone());
            state.save()?;
            Ok(Some(serde_json::to_value(account).map_err(|err| RpcError {
                code: -32603,
                message: format!("encode failed: {err}"),
            })?))
        }
        "login.start" => {
            let params = parse_params::<LoginStartParams>(request.params.as_ref())?;
            let mut state = state.lock().await;
            let account = state
                .accounts
                .iter_mut()
                .find(|acct| acct.id == params.account_id)
                .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "unknown account".to_string(),
                })?;

            account.status = AccountStatus::LoginInProgress;
            state.save()?;

            let mut response = serde_json::Map::new();
            response.insert("account_id".to_string(), serde_json::Value::String(account.id.clone()));
            response.insert(
                "login_method".to_string(),
                serde_json::Value::String(match account.login_method {
                    LoginMethod::Password => "password",
                    LoginMethod::Sso => "sso",
                }
                .to_string()),
            );

            if matches!(account.login_method, LoginMethod::Sso) {
                let sso_url = format!(
                    "{}/_matrix/client/v3/login/sso/redirect",
                    account.homeserver.trim_end_matches('/')
                );
                response.insert("sso_url".to_string(), serde_json::Value::String(sso_url));
                response.insert(
                    "redirect_url".to_string(),
                    serde_json::Value::String("http://127.0.0.1:9388/callback".to_string()),
                );
            }

            Ok(Some(serde_json::Value::Object(response)))
        }
        "login.complete" => {
            let params = parse_params::<LoginCompleteParams>(request.params.as_ref())?;
            let mut state = state.lock().await;
            let account = state
                .accounts
                .iter_mut()
                .find(|acct| acct.id == params.account_id)
                .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "unknown account".to_string(),
                })?;

            let _token = params.token;
            account.status = AccountStatus::Ready;
            state.save()?;
            Ok(Some(serde_json::json!({
                "account_id": account.id,
                "status": "ready"
            })))
        }
        "matrix.login_flows" => {
            let params = parse_params::<MatrixHomeserverParams>(request.params.as_ref())?;
            let homeserver = normalize_homeserver(&params.homeserver);
            let login = fetch_ruma_json::<
                matrix_sdk::ruma::api::client::session::get_login_types::v3::Response,
            >(
                &homeserver,
                "/_matrix/client/v3/login",
            )
            .await?;
            let mut response = serde_json::Map::new();
            response.insert("homeserver".to_string(), serde_json::Value::String(homeserver));
            response.insert("login".to_string(), login.value);
            if let Some(warning) = login.parse_warning {
                response.insert(
                    "parse_warning".to_string(),
                    serde_json::Value::String(warning),
                );
            }
            Ok(Some(serde_json::Value::Object(response)))
        }
        "matrix.capabilities" => {
            let params = parse_params::<MatrixHomeserverParams>(request.params.as_ref())?;
            let homeserver = normalize_homeserver(&params.homeserver);
            let capabilities = fetch_ruma_json::<
                matrix_sdk::ruma::api::client::capabilities::get_capabilities::v3::Response,
            >(&homeserver, "/_matrix/client/v3/capabilities")
            .await?;
            let mut response = serde_json::Map::new();
            response.insert("homeserver".to_string(), serde_json::Value::String(homeserver));
            response.insert("capabilities".to_string(), capabilities.value);
            if let Some(warning) = capabilities.parse_warning {
                response.insert(
                    "parse_warning".to_string(),
                    serde_json::Value::String(warning),
                );
            }
            Ok(Some(serde_json::Value::Object(response)))
        }
        _ => Err(RpcError {
            code: -32601,
            message: "method not found".to_string(),
        }),
    }
}

fn parse_params<T>(params: Option<&serde_json::Value>) -> Result<T, RpcError>
where
    T: for<'de> Deserialize<'de>,
{
    let params = params.ok_or_else(|| RpcError {
        code: -32602,
        message: "missing params".to_string(),
    })?;
    serde_json::from_value(params.clone()).map_err(|err| RpcError {
        code: -32602,
        message: format!("invalid params: {err}"),
    })
}

fn normalize_homeserver(homeserver: &str) -> String {
    if homeserver.starts_with("http://") || homeserver.starts_with("https://") {
        homeserver.to_string()
    } else {
        format!("https://{homeserver}")
    }
}

async fn fetch_ruma_json<T>(homeserver: &str, path: &str) -> Result<FetchResult, RpcError>
where
    T: DeserializeOwned,
{
    let url = format!("{}{}", homeserver.trim_end_matches('/'), path);
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|err| RpcError {
            code: -32603,
            message: format!("request failed: {err}"),
        })?;

    if !response.status().is_success() {
        return Err(RpcError {
            code: -32603,
            message: format!("request failed: {}", response.status()),
        });
    }

    let body = response.text().await.map_err(|err| RpcError {
        code: -32603,
        message: format!("read failed: {err}"),
    })?;

    let raw_value = serde_json::from_str::<serde_json::Value>(&body).map_err(|err| RpcError {
        code: -32603,
        message: format!("invalid json: {err}"),
    })?;

    match serde_json::from_value::<T>(raw_value.clone()) {
        Ok(typed) => {
            let value = serde_json::to_value(typed).map_err(|err| RpcError {
                code: -32603,
                message: format!("encode failed: {err}"),
            })?;
            Ok(FetchResult {
                value,
                parse_warning: None,
            })
        }
        Err(err) => Ok(FetchResult {
            value: raw_value,
            parse_warning: Some(format!(
                "non-spec response; fallback to raw JSON ({err})"
            )),
        }),
    }
}
