use crate::state::{Account, AccountStatus, LoginMethod, State};
use anyhow::Result;
use serde::Deserialize;
use ruma_common::api::IncomingResponse;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    method: Option<String>,
    params: Option<serde_json::Value>,
    id: Option<serde_json::Value>,
}

#[derive(Debug, serde::Serialize)]
pub struct RpcError {
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

pub async fn run_rpc_server(
    addr: &str,
    state: std::sync::Arc<tokio::sync::Mutex<State>>,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<()> {
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
) -> Result<()> {
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
            Ok(Some(serde_json::to_value(&state.accounts).map_err(internal_error)?))
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
            state.save().map_err(internal_error)?;
            Ok(Some(serde_json::to_value(account).map_err(internal_error)?))
        }
        "login.start" => {
            let params = parse_params::<LoginStartParams>(request.params.as_ref())?;
            let mut state = state.lock().await;
            let (account_id, login_method, homeserver) = {
                let account = state
                    .accounts
                    .iter_mut()
                    .find(|acct| acct.id == params.account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;

                account.status = AccountStatus::LoginInProgress;
                (
                    account.id.clone(),
                    account.login_method.clone(),
                    account.homeserver.clone(),
                )
            };
            state.save().map_err(internal_error)?;

            let mut response = serde_json::Map::new();
            response.insert("account_id".to_string(), serde_json::Value::String(account_id.clone()));
            response.insert(
                "login_method".to_string(),
                serde_json::Value::String(match login_method {
                    LoginMethod::Password => "password",
                    LoginMethod::Sso => "sso",
                }
                .to_string()),
            );

            if matches!(login_method, LoginMethod::Sso) {
                let sso_url = format!(
                    "{}/_matrix/client/v3/login/sso/redirect",
                    homeserver.trim_end_matches('/')
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
            let account_id = {
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
                account.id.clone()
            };
            state.save().map_err(internal_error)?;
            Ok(Some(serde_json::json!({
                "account_id": account_id,
                "status": "ready"
            })))
        }
        "matrix.login_flows" => {
            let params = parse_params::<MatrixHomeserverParams>(request.params.as_ref())?;
            let homeserver = normalize_homeserver(&params.homeserver);
            let login = fetch_ruma_json::<
                ruma_client_api::session::get_login_types::v3::Response,
            >(
                &homeserver,
                "/_matrix/client/v3/login",
            )
            .await?;
            Ok(Some(matrix_response(homeserver, "login", login)))
        }
        "matrix.capabilities" => {
            let params = parse_params::<MatrixHomeserverParams>(request.params.as_ref())?;
            let homeserver = normalize_homeserver(&params.homeserver);
            let capabilities = fetch_ruma_json::<
                ruma_client_api::discovery::get_capabilities::v3::Response,
            >(&homeserver, "/_matrix/client/v3/capabilities")
            .await?;
            Ok(Some(matrix_response(
                homeserver,
                "capabilities",
                capabilities,
            )))
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

fn matrix_response(homeserver: String, key: &str, result: FetchResult) -> serde_json::Value {
    let mut response = serde_json::Map::new();
    response.insert("homeserver".to_string(), serde_json::Value::String(homeserver));
    response.insert(key.to_string(), result.value);
    if let Some(warning) = result.parse_warning {
        response.insert(
            "parse_warning".to_string(),
            serde_json::Value::String(warning),
        );
    }
    serde_json::Value::Object(response)
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
    T: IncomingResponse,
{
    let url = format!("{}{}", homeserver.trim_end_matches('/'), path);
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(internal_error)?;

    if !response.status().is_success() {
        return Err(RpcError {
            code: -32603,
            message: format!("request failed: {}", response.status()),
        });
    }

    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(internal_error)?;

    let raw_value =
        serde_json::from_slice::<serde_json::Value>(&body).map_err(internal_error)?;

    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    let http_response = builder
        .body(body.to_vec())
        .map_err(internal_error)?;

    match T::try_from_http_response(http_response) {
        Ok(_) => Ok(FetchResult {
            value: raw_value,
            parse_warning: None,
        }),
        Err(err) => Ok(FetchResult {
            value: raw_value,
            parse_warning: Some(format!(
                "non-spec response; fallback to raw JSON ({err})"
            )),
        }),
    }
}

fn internal_error<E: std::fmt::Display>(err: E) -> RpcError {
    RpcError {
        code: -32603,
        message: format!("internal error: {err}"),
    }
}
