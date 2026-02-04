use crate::events::EventBus;
use crate::matrix::{self, MatrixRuntime, VerificationSnapshot};
use crate::state::{Account, AccountStatus, LoginMethod, State};
use anyhow::Result;
use matrix_sdk::stream::StreamExt;
use matrix_sdk::encryption::verification::SasState;
use matrix_sdk::ruma::{OwnedDeviceId, UserId};
use ruma_common::api::IncomingResponse;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use std::sync::atomic::{AtomicU64, Ordering};

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
    login_method: Option<LoginMethod>,
}

#[derive(Debug, Deserialize)]
struct EventsSubscribeParams {
    account_id: String,
    since: Option<String>,
    snapshot: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct EventsUnsubscribeParams {
    subscription_id: String,
}

#[derive(Debug, Deserialize)]
struct MatrixHomeserverParams {
    homeserver: String,
}

#[derive(Debug, Deserialize)]
struct MatrixDevicesListParams {
    account_id: String,
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MatrixVerificationListParams {
    account_id: String,
    user_id: Option<String>,
}

#[derive(Debug)]
struct FetchResult {
    value: serde_json::Value,
    parse_warning: Option<String>,
}

static SUB_ID: AtomicU64 = AtomicU64::new(1);

struct Subscription {
    subscription_id: String,
    task: tokio::task::JoinHandle<()>,
}

pub async fn run_rpc_server(
    addr: &str,
    state: std::sync::Arc<tokio::sync::Mutex<State>>,
    runtime: std::sync::Arc<tokio::sync::Mutex<MatrixRuntime>>,
    event_bus: std::sync::Arc<EventBus>,
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
                let client_runtime = runtime.clone();
                let client_events = event_bus.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        handle_client(stream, client_state, client_runtime, client_events).await
                    {
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
    runtime: std::sync::Arc<tokio::sync::Mutex<MatrixRuntime>>,
    event_bus: std::sync::Arc<EventBus>,
) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut write_half = write_half;
        while let Some(line) = out_rx.recv().await {
            write_half.write_all(line.as_bytes()).await?;
            write_half.write_all(b"\n").await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    let mut subscription: Option<Subscription> = None;

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
                let _ = out_tx.send(payload);
                continue;
            }
        };

        let method = match request.method.as_deref() {
            Some(value) => value,
            None => {
                let resp = RpcResponse {
                    jsonrpc: "2.0",
                    result: None,
                    error: Some(RpcError {
                        code: -32600,
                        message: "invalid request".to_string(),
                    }),
                    id: request.id.clone().unwrap_or(serde_json::Value::Null),
                };
                let payload = serde_json::to_string(&resp)?;
                let _ = out_tx.send(payload);
                continue;
            }
        };

        if method == "events.subscribe" {
            let response = match handle_events_subscribe(
                &request,
                &state,
                &runtime,
                &event_bus,
                &out_tx,
                &mut subscription,
            )
            .await
            {
                Ok(resp) => resp,
                Err(err) => Some(RpcResponse {
                    jsonrpc: "2.0",
                    result: None,
                    error: Some(err),
                    id: request.id.clone().unwrap_or(serde_json::Value::Null),
                }),
            };
            if let Some(resp) = response {
                let payload = serde_json::to_string(&resp)?;
                let _ = out_tx.send(payload);
            }
            continue;
        }

        if method == "events.unsubscribe" {
            let response = match handle_events_unsubscribe(&request, &mut subscription) {
                Ok(resp) => resp,
                Err(err) => Some(RpcResponse {
                    jsonrpc: "2.0",
                    result: None,
                    error: Some(err),
                    id: request.id.clone().unwrap_or(serde_json::Value::Null),
                }),
            };
            if let Some(resp) = response {
                let payload = serde_json::to_string(&resp)?;
                let _ = out_tx.send(payload);
            }
            continue;
        }

        let response = match handle_request(&request, &state, &runtime, &event_bus).await {
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
                let _ = out_tx.send(payload);
            }
        }
    }

    if let Some(active) = subscription {
        active.task.abort();
    }
    drop(out_tx);
    let _ = writer.await;

    Ok(())
}

async fn handle_events_subscribe(
    request: &RpcRequest,
    state: &std::sync::Arc<tokio::sync::Mutex<State>>,
    runtime: &std::sync::Arc<tokio::sync::Mutex<MatrixRuntime>>,
    event_bus: &std::sync::Arc<EventBus>,
    out_tx: &mpsc::UnboundedSender<String>,
    subscription: &mut Option<Subscription>,
) -> Result<Option<RpcResponse>, RpcError> {
    if request.jsonrpc.as_deref() != Some("2.0") {
        return Err(RpcError {
            code: -32600,
            message: "invalid request".to_string(),
        });
    }

    let params = parse_params::<EventsSubscribeParams>(request.params.as_ref())?;
    if let Some(active) = subscription.take() {
        active.task.abort();
    }

    let subscription_id = format!("sub-{}", SUB_ID.fetch_add(1, Ordering::SeqCst));
    let account_id = params.account_id.clone();

    let mut rx = event_bus.subscribe();
    let out_tx_clone = out_tx.clone();
    let subscription_id_clone = subscription_id.clone();
    let account_id_clone = account_id.clone();
    let event_bus_clone = event_bus.clone();

    let task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.account_id != account_id_clone {
                        continue;
                    }
                    let _ = send_event_push(&out_tx_clone, &subscription_id_clone, &event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let latest = event_bus_clone
                        .latest_cursor(&account_id_clone)
                        .await
                        .unwrap_or_else(|| "cursor-0".to_string());
                    let _ = send_event_push_raw(
                        &out_tx_clone,
                        &subscription_id_clone,
                        &account_id_clone,
                        "events.reset",
                        0,
                        &latest,
                        serde_json::json!({
                            "reason": "cursor_too_old",
                            "new_since": latest,
                        }),
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    *subscription = Some(Subscription {
        subscription_id: subscription_id.clone(),
        task,
    });

    if let Some(since) = params.since.as_deref() {
        if let Some(events) = event_bus.replay_since(&account_id, since).await {
            for event in events {
                let _ = send_event_push(out_tx, &subscription_id, &event);
            }
        } else {
            let latest = event_bus
                .latest_cursor(&account_id)
                .await
                .unwrap_or_else(|| "cursor-0".to_string());
            let _ = send_event_push_raw(
                out_tx,
                &subscription_id,
                &account_id,
                "events.reset",
                0,
                &latest,
                serde_json::json!({
                    "reason": "invalid_cursor",
                    "new_since": latest,
                }),
            );
        }
    }

    if params.snapshot.unwrap_or(true) {
        send_snapshot_events(out_tx, state, runtime, event_bus, &account_id, &subscription_id)
            .await?;
    }

    let since = event_bus
        .latest_cursor(&account_id)
        .await
        .unwrap_or_else(|| "cursor-0".to_string());

    Ok(Some(RpcResponse {
        jsonrpc: "2.0",
        result: Some(serde_json::json!({
            "subscription_id": subscription_id,
            "account_id": account_id,
            "since": since,
        })),
        error: None,
        id: request.id.clone().unwrap_or(serde_json::Value::Null),
    }))
}

fn handle_events_unsubscribe(
    request: &RpcRequest,
    subscription: &mut Option<Subscription>,
) -> Result<Option<RpcResponse>, RpcError> {
    if request.jsonrpc.as_deref() != Some("2.0") {
        return Err(RpcError {
            code: -32600,
            message: "invalid request".to_string(),
        });
    }
    let params = parse_params::<EventsUnsubscribeParams>(request.params.as_ref())?;
    if let Some(active) = subscription.take() {
        if active.subscription_id == params.subscription_id {
            active.task.abort();
        } else {
            *subscription = Some(active);
        }
    }
    Ok(Some(RpcResponse {
        jsonrpc: "2.0",
        result: Some(serde_json::json!({ "ok": true })),
        error: None,
        id: request.id.clone().unwrap_or(serde_json::Value::Null),
    }))
}

async fn handle_request(
    request: &RpcRequest,
    state: &std::sync::Arc<tokio::sync::Mutex<State>>,
    runtime: &std::sync::Arc<tokio::sync::Mutex<MatrixRuntime>>,
    event_bus: &std::sync::Arc<EventBus>,
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
            let homeserver = resolve_homeserver(&params.homeserver).await?;
            let login = fetch_ruma_json::<
                ruma_client_api::session::get_login_types::v3::Response,
            >(
                &homeserver,
                "/_matrix/client/v3/login",
            )
            .await?;
            let available = extract_login_methods(&login.value);

            if let Some(method) = params.login_method.clone() {
                if !available.contains(&method) {
                    return Err(RpcError {
                        code: -32602,
                        message: "unsupported login method".to_string(),
                    });
                }
            }

            let selected = if let Some(method) = params.login_method.clone() {
                Some(method)
            } else if available.len() == 1 {
                Some(available[0].clone())
            } else {
                None
            };

            let mut state = state.lock().await;
            let id = format!("acct-{}", state.next_id);
            state.next_id += 1;
            let account = Account {
                id: id.clone(),
                homeserver: homeserver.clone(),
                login_method: selected,
                status: AccountStatus::NeedsLogin,
                session: None,
            };
            state.accounts.push(account.clone());
            state.save().map_err(internal_error)?;
            drop(state);
            event_bus
                .emit(
                    &account.id,
                    "account.state",
                    serde_json::json!({
                        "account_id": account.id,
                        "status": status_to_string(&account.status),
                        "has_session": account.session.is_some(),
                    }),
                )
                .await;
            let mut response = serde_json::to_value(account).map_err(internal_error)?;
            if let serde_json::Value::Object(ref mut obj) = response {
                obj.insert(
                    "available_login_methods".to_string(),
                    serde_json::to_value(&available).map_err(internal_error)?,
                );
                obj.insert("login_flows".to_string(), login.value);
                if let Some(warn) = login.parse_warning {
                    obj.insert("parse_warning".to_string(), serde_json::Value::String(warn));
                }
            }
            Ok(Some(response))
        }
        "account.login_password" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                username: String,
                password: String,
                device_name: Option<String>,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let (account_id, homeserver, state_dir) = {
                let state = state.lock().await;
                let account = state
                    .accounts
                    .iter()
                    .find(|acct| acct.id == params.account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;
                (
                    account.id.clone(),
                    account.homeserver.clone(),
                    state.state_dir().to_path_buf(),
                )
            };

            let store_dir = matrix::account_store_dir(&state_dir, &account_id);
            let client = matrix::build_client(&homeserver, &store_dir)
                .await
                .map_err(internal_error)?;
            let session = matrix::login_password(
                &client,
                &params.username,
                &params.password,
                params.device_name,
            )
            .await
            .map_err(internal_error)?;

            {
                let mut state = state.lock().await;
                let account = state
                    .accounts
                    .iter_mut()
                    .find(|acct| acct.id == account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;
                account.login_method = Some(LoginMethod::Password);
                account.session = Some(session);
                account.status = AccountStatus::Ready;
                state.save().map_err(internal_error)?;
            }

            let mut runtime = runtime.lock().await;
            runtime.insert_client(account_id.clone(), client.clone());
            runtime.start_sync(account_id.clone(), client);

            let result = serde_json::json!({
                "account_id": account_id,
                "status": "ready"
            });
            let account_snapshot = {
                let state = state.lock().await;
                state.accounts.iter().find(|acct| acct.id == account_id).cloned()
            };
            if let Some(account) = account_snapshot {
                event_bus
                    .emit(
                        &account.id,
                        "account.state",
                        serde_json::json!({
                            "account_id": account.id,
                            "status": status_to_string(&account.status),
                            "has_session": account.session.is_some(),
                        }),
                    )
                    .await;
                if let Some(session) = &account.session {
                    event_bus
                        .emit(
                            &account.id,
                            "matrix.login.ready",
                            serde_json::json!({
                                "status":"ready",
                                "device_id": session.device_id,
                                "user_id": session.user_id,
                            }),
                        )
                        .await;
                }
            }
            Ok(Some(result))
        }
        "account.login_complete" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                token: String,
                device_name: Option<String>,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let (account_id, homeserver, state_dir) = {
                let state = state.lock().await;
                let account = state
                    .accounts
                    .iter()
                    .find(|acct| acct.id == params.account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;
                (
                    account.id.clone(),
                    account.homeserver.clone(),
                    state.state_dir().to_path_buf(),
                )
            };

            let store_dir = matrix::account_store_dir(&state_dir, &account_id);
            let client = matrix::build_client(&homeserver, &store_dir)
                .await
                .map_err(internal_error)?;
            let session =
                matrix::login_token(&client, &params.token, params.device_name)
                    .await
                    .map_err(internal_error)?;

            {
                let mut state = state.lock().await;
                let account = state
                    .accounts
                    .iter_mut()
                    .find(|acct| acct.id == account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;
                account.login_method = Some(LoginMethod::Sso);
                account.session = Some(session);
                account.status = AccountStatus::Ready;
                state.save().map_err(internal_error)?;
            }

            let mut runtime = runtime.lock().await;
            runtime.insert_client(account_id.clone(), client.clone());
            runtime.start_sync(account_id.clone(), client);

            let result = serde_json::json!({
                "account_id": account_id,
                "status": "ready"
            });
            let account_snapshot = {
                let state = state.lock().await;
                state.accounts.iter().find(|acct| acct.id == account_id).cloned()
            };
            if let Some(account) = account_snapshot {
                event_bus
                    .emit(
                        &account.id,
                        "account.state",
                        serde_json::json!({
                            "account_id": account.id,
                            "status": status_to_string(&account.status),
                            "has_session": account.session.is_some(),
                        }),
                    )
                    .await;
                if let Some(session) = &account.session {
                    event_bus
                        .emit(
                            &account.id,
                            "matrix.login.ready",
                            serde_json::json!({
                                "status":"ready",
                                "device_id": session.device_id,
                                "user_id": session.user_id,
                            }),
                        )
                        .await;
                }
            }
            Ok(Some(result))
        }
        "account.session_restore" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let (account_id, homeserver, state_dir, session) = {
                let state = state.lock().await;
                let account = state
                    .accounts
                    .iter()
                    .find(|acct| acct.id == params.account_id)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown account".to_string(),
                    })?;
                let session = account.session.clone().ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no stored session".to_string(),
                })?;
                (
                    account.id.clone(),
                    account.homeserver.clone(),
                    state.state_dir().to_path_buf(),
                    session,
                )
            };

            let store_dir = matrix::account_store_dir(&state_dir, &account_id);
            let client = matrix::build_client(&homeserver, &store_dir)
                .await
                .map_err(internal_error)?;
            let sdk_session = matrix::sdk_session_from_stored(&session)
                .map_err(internal_error)?;
            matrix::restore_session(&client, sdk_session)
                .await
                .map_err(internal_error)?;

            let mut runtime = runtime.lock().await;
            runtime.insert_client(account_id.clone(), client.clone());
            runtime.start_sync(account_id.clone(), client);

            let result = serde_json::json!({
                "account_id": account_id,
                "status": "ready"
            });
            let account_snapshot = {
                let state = state.lock().await;
                state.accounts.iter().find(|acct| acct.id == account_id).cloned()
            };
            if let Some(account) = account_snapshot {
                event_bus
                    .emit(
                        &account.id,
                        "account.state",
                        serde_json::json!({
                            "account_id": account.id,
                            "status": status_to_string(&account.status),
                            "has_session": account.session.is_some(),
                        }),
                    )
                    .await;
                if let Some(session) = &account.session {
                    event_bus
                        .emit(
                            &account.id,
                            "matrix.login.ready",
                            serde_json::json!({
                                "status":"ready",
                                "device_id": session.device_id,
                                "user_id": session.user_id,
                            }),
                        )
                        .await;
                }
            }
            Ok(Some(result))
        }
        "room.list" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let rooms = matrix::list_rooms(&client)
                .await
                .map_err(internal_error)?;
            Ok(Some(serde_json::json!({ "rooms": rooms })))
        }
        "room.messages" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                room_id: String,
                limit: Option<usize>,
                from: Option<String>,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let limit = params.limit.unwrap_or(20);
            let messages = matrix::fetch_messages(&client, &params.room_id, limit, params.from)
                .await
                .map_err(internal_error)?;
            Ok(Some(serde_json::to_value(messages).map_err(internal_error)?))
        }
        "room.send" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                room_id: String,
                body: String,
                txn_id: Option<String>,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let event_id = matrix::send_message_text(
                &client,
                &params.room_id,
                &params.body,
                params.txn_id.clone(),
            )
            .await
            .map_err(internal_error)?;
            Ok(Some(serde_json::json!({
                "event_id": event_id,
                "txn_id": params.txn_id,
            })))
        }
        "matrix.verification.request" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                user_id: String,
                device_id: Option<String>,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let user_id = if params.user_id == "me" || params.user_id == "self" {
                client.user_id().ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no session user id".to_string(),
                })?.to_string()
            } else {
                params.user_id.clone()
            };
            if params.device_id.is_none() {
                if let Some(own_user_id) = client.user_id() {
                    if own_user_id.as_str() == user_id {
                        return Err(RpcError {
                            code: -32602,
                            message: "device_id required for self verification".to_string(),
                        });
                    }
                }
            }

            let verification = if let Some(device_id) = params.device_id.as_deref() {
                let user_id = UserId::parse(&user_id).map_err(internal_error)?;
                let device_id = OwnedDeviceId::try_from(device_id)
                    .map_err(internal_error)?;
                let device = client
                    .encryption()
                    .get_device(&user_id, device_id.as_ref())
                    .await
                    .map_err(internal_error)?
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown device".to_string(),
                    })?;
                device.request_verification().await.map_err(internal_error)?
            } else {
                let user_id = UserId::parse(&user_id).map_err(internal_error)?;
                let identity = client
                    .encryption()
                    .get_user_identity(&user_id)
                    .await
                    .map_err(internal_error)?
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "unknown user".to_string(),
                    })?;
                identity.request_verification().await.map_err(internal_error)?
            };

            let flow_id = verification.flow_id().to_string();
            let user_id = verification.other_user_id().to_string();
            let result = serde_json::json!({
                "flow_id": verification.flow_id(),
                "user_id": verification.other_user_id(),
                "state": format!("{:?}", verification.state()).to_lowercase()
            });
            let verifications = {
                let runtime = runtime.lock().await;
                runtime.verifications_handle()
            };
            {
                let mut guard = verifications.lock().await;
                guard
                    .entry(params.account_id.clone())
                    .or_insert_with(std::collections::HashMap::new)
                    .insert(
                        flow_id.clone(),
                        VerificationSnapshot {
                            flow_id: flow_id.clone(),
                            user_id: user_id.clone(),
                            device_id: params.device_id.clone(),
                            stage: "requested".to_string(),
                            sas: None,
                        },
                    );
            }
            event_bus
                .emit(
                    &params.account_id,
                    "matrix.verification.state",
                    serde_json::json!({
                        "flow_id": flow_id,
                        "user_id": user_id,
                        "device_id": params.device_id,
                        "stage": "requested",
                    }),
                )
                .await;
            Ok(Some(result))
        }
        "matrix.verification.accept" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                user_id: String,
                flow_id: String,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let user_id = resolve_user_id_param(&client, &params.user_id)?;
            let request = matrix::get_verification_request(
                &client,
                &user_id,
                &params.flow_id,
            )
            .await
            .map_err(internal_error)?
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "unknown verification".to_string(),
            })?;
            request.accept().await.map_err(internal_error)?;
            let verifications = {
                let runtime = runtime.lock().await;
                runtime.verifications_handle()
            };
            {
                let mut guard = verifications.lock().await;
                guard
                    .entry(params.account_id.clone())
                    .or_insert_with(std::collections::HashMap::new)
                    .insert(
                        params.flow_id.clone(),
                        VerificationSnapshot {
                            flow_id: params.flow_id.clone(),
                            user_id: params.user_id.clone(),
                            device_id: None,
                            stage: "accepted".to_string(),
                            sas: None,
                        },
                    );
            }
            event_bus
                .emit(
                    &params.account_id,
                    "matrix.verification.state",
                    serde_json::json!({
                        "flow_id": params.flow_id,
                        "user_id": user_id,
                        "stage": "accepted",
                    }),
                )
                .await;

            {
                let event_bus = event_bus.clone();
                let verifications = {
                    let runtime = runtime.lock().await;
                    runtime.verifications_handle()
                };
                let account_id = params.account_id.clone();
                let user_id = user_id.clone();
                let flow_id = params.flow_id.clone();
                let request = request.clone();
                tokio::spawn(async move {
                    let mut changes = request.changes();
                    while let Some(state) = changes.next().await {
                        use matrix_sdk::encryption::verification::{Verification, VerificationRequestState};
                        match state {
                            VerificationRequestState::Ready { .. } => {
                                if let Ok(Some(sas)) = request.start_sas().await {
                                    let _ = sas.accept().await;
                                    let event_bus = event_bus.clone();
                                    let verifications = verifications.clone();
                                    let account_id = account_id.clone();
                                    let flow_id = flow_id.clone();
                                    let user_id = user_id.clone();
                                    tokio::spawn(async move {
                                        emit_sas_when_ready(
                                            event_bus,
                                            verifications,
                                            account_id,
                                            flow_id,
                                            user_id,
                                            sas,
                                        )
                                        .await;
                                    });
                                }
                            }
                            VerificationRequestState::Transitioned { verification } => {
                                if let Verification::SasV1(sas) = verification {
                                    let _ = sas.accept().await;
                                    let event_bus = event_bus.clone();
                                    let verifications = verifications.clone();
                                    let account_id = account_id.clone();
                                    let flow_id = flow_id.clone();
                                    let user_id = user_id.clone();
                                    tokio::spawn(async move {
                                        emit_sas_when_ready(
                                            event_bus,
                                            verifications,
                                            account_id,
                                            flow_id,
                                            user_id,
                                            sas,
                                        )
                                        .await;
                                    });
                                }
                            }
                            VerificationRequestState::Done => {
                                let removed = {
                                    let mut guard = verifications.lock().await;
                                    guard
                                        .get_mut(&account_id)
                                        .and_then(|entries| entries.remove(&flow_id))
                                        .is_some()
                                };
                                if removed {
                                    event_bus
                                        .emit(
                                            &account_id,
                                            "matrix.verification.done",
                                            serde_json::json!({
                                                "flow_id": flow_id,
                                                "user_id": user_id,
                                                "reason": "verified",
                                            }),
                                        )
                                        .await;
                                }
                                break;
                            }
                            VerificationRequestState::Cancelled(info) => {
                                let removed = {
                                    let mut guard = verifications.lock().await;
                                    guard
                                        .get_mut(&account_id)
                                        .and_then(|entries| entries.remove(&flow_id))
                                        .is_some()
                                };
                                if removed {
                                    event_bus
                                        .emit(
                                            &account_id,
                                            "matrix.verification.cancelled",
                                            serde_json::json!({
                                                "flow_id": flow_id,
                                                "user_id": user_id,
                                                "reason": format!("{info:?}"),
                                            }),
                                        )
                                        .await;
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                });
            }

            Ok(Some(serde_json::json!({
                "flow_id": params.flow_id,
                "status": "accepted"
            })))
        }
        "matrix.verification.confirm" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                user_id: String,
                flow_id: String,
                #[serde(rename = "match")]
                match_: bool,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let user_id = resolve_user_id_param(&client, &params.user_id)?;
            let sas = matrix::get_sas_verification(&client, &user_id, &params.flow_id)
                .await
                .map_err(internal_error)?
                .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "unknown sas".to_string(),
                })?;
            if params.match_ {
                sas.confirm().await.map_err(internal_error)?;
            } else {
                sas.mismatch().await.map_err(internal_error)?;
            }
            if params.match_ {
                event_bus
                    .emit(
                        &params.account_id,
                        "matrix.verification.done",
                        serde_json::json!({
                            "flow_id": params.flow_id,
                            "user_id": user_id,
                            "reason": "verified",
                        }),
                    )
                    .await;
            } else {
                event_bus
                    .emit(
                        &params.account_id,
                        "matrix.verification.cancelled",
                        serde_json::json!({
                            "flow_id": params.flow_id,
                            "user_id": user_id,
                            "reason": "mismatch",
                        }),
                    )
                    .await;
            }
            let verifications = {
                let runtime = runtime.lock().await;
                runtime.verifications_handle()
            };
            {
                let mut guard = verifications.lock().await;
                if let Some(entries) = guard.get_mut(&params.account_id) {
                    entries.remove(&params.flow_id);
                }
            }
            Ok(Some(serde_json::json!({
                "flow_id": params.flow_id,
                "status": if params.match_ { "confirmed" } else { "mismatch" }
            })))
        }
        "matrix.verification.cancel" => {
            #[derive(Deserialize)]
            struct Params {
                account_id: String,
                user_id: String,
                flow_id: String,
            }
            let params = parse_params::<Params>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let user_id = resolve_user_id_param(&client, &params.user_id)?;
            if let Some(request) = matrix::get_verification_request(
                &client,
                &user_id,
                &params.flow_id,
            )
            .await
            .map_err(internal_error)?
            {
                request.cancel().await.map_err(internal_error)?;
            }
            event_bus
                    .emit(
                        &params.account_id,
                        "matrix.verification.cancelled",
                        serde_json::json!({
                            "flow_id": params.flow_id,
                            "user_id": user_id,
                            "reason": "user_cancelled",
                        }),
                    )
                    .await;
            let verifications = {
                let runtime = runtime.lock().await;
                runtime.verifications_handle()
            };
            {
                let mut guard = verifications.lock().await;
                if let Some(entries) = guard.get_mut(&params.account_id) {
                    entries.remove(&params.flow_id);
                }
            }
            Ok(Some(serde_json::json!({
                "flow_id": params.flow_id,
                "status": "cancelled"
            })))
        }
        "account.login_start" => {
            let params = parse_params::<LoginStartParams>(request.params.as_ref())?;
            let mut state_guard = state.lock().await;
            let (account_id, stored_method, homeserver) = {
                let account = state_guard
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
            state_guard.save().map_err(internal_error)?;
            let account_snapshot = state_guard
                .accounts
                .iter()
                .find(|acct| acct.id == account_id)
                .cloned();
            drop(state_guard);
            if let Some(account) = account_snapshot {
                event_bus
                    .emit(
                        &account.id,
                        "account.state",
                        serde_json::json!({
                            "account_id": account.id,
                            "status": status_to_string(&account.status),
                            "has_session": account.session.is_some(),
                        }),
                    )
                    .await;
            }

            let homeserver = resolve_homeserver(&homeserver).await?;
            let login = fetch_ruma_json::<
                ruma_client_api::session::get_login_types::v3::Response,
            >(
                &homeserver,
                "/_matrix/client/v3/login",
            )
            .await?;
            let available = extract_login_methods(&login.value);

            let login_method = if let Some(method) = params.login_method.clone() {
                if !available.contains(&method) {
                    return Err(RpcError {
                        code: -32602,
                        message: "unsupported login method".to_string(),
                    });
                }
                method
            } else if let Some(method) = stored_method.clone() {
                if !available.contains(&method) {
                    return Err(RpcError {
                        code: -32602,
                        message: "unsupported login method".to_string(),
                    });
                }
                method
            } else if available.len() == 1 {
                available[0].clone()
            } else {
                return Err(RpcError {
                    code: -32602,
                    message: "login method required".to_string(),
                });
            };

            if stored_method.as_ref() != Some(&login_method) {
                let mut state_guard = state.lock().await;
                if let Some(account) = state_guard
                    .accounts
                    .iter_mut()
                    .find(|acct| acct.id == account_id)
                {
                    account.login_method = Some(login_method.clone());
                    state_guard.save().map_err(internal_error)?;
                }
            }

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
        "matrix.login_flows" => {
            let params = parse_params::<MatrixHomeserverParams>(request.params.as_ref())?;
            let homeserver = resolve_homeserver(&params.homeserver).await?;
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
            let homeserver = resolve_homeserver(&params.homeserver).await?;
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
        "matrix.devices.list" => {
            let params = parse_params::<MatrixDevicesListParams>(request.params.as_ref())?;
            let client = {
                let runtime = runtime.lock().await;
                runtime.get_client(&params.account_id).ok_or_else(|| RpcError {
                    code: -32602,
                    message: "no active session".to_string(),
                })?
            };
            let user_id = if let Some(user_id) = params.user_id {
                UserId::parse(&user_id).map_err(internal_error)?
            } else {
                client
                    .user_id()
                    .map(|user_id| user_id.to_owned())
                    .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "missing user id".to_string(),
                })?
            };
            let devices = client
                .encryption()
                .get_user_devices(&user_id)
                .await
                .map_err(internal_error)?;
            let items: Vec<_> = devices
                .devices()
                .map(|device| {
                    serde_json::json!({
                        "user_id": device.user_id().to_string(),
                        "device_id": device.device_id().to_string(),
                        "display_name": device.display_name(),
                        "is_verified": device.is_verified(),
                    })
                })
                .collect();
            Ok(Some(serde_json::Value::Array(items)))
        }
        "matrix.verification.list" => {
            let params = parse_params::<MatrixVerificationListParams>(request.params.as_ref())?;
            let verifications_handle = {
                let runtime = runtime.lock().await;
                runtime.verifications_handle()
            };
            let items = {
                let guard = verifications_handle.lock().await;
                guard
                    .get(&params.account_id)
                    .map(|entries| {
                        entries
                            .values()
                            .filter(|entry| {
                                params
                                    .user_id
                                    .as_ref()
                                    .map(|user_id| &entry.user_id == user_id)
                                    .unwrap_or(true)
                            })
                            .map(|entry| {
                                let short_id = if entry.flow_id.len() > 8 {
                                    entry.flow_id[..8].to_string()
                                } else {
                                    entry.flow_id.clone()
                                };
                                serde_json::json!({
                                    "flow_id": entry.flow_id,
                                    "short_id": short_id,
                                    "user_id": entry.user_id,
                                    "device_id": entry.device_id,
                                    "stage": entry.stage,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            Ok(Some(serde_json::Value::Array(items)))
        }
        _ => Err(RpcError {
            code: -32601,
            message: "method not found".to_string(),
        }),
    }
}

async fn send_snapshot_events(
    out_tx: &mpsc::UnboundedSender<String>,
    state: &std::sync::Arc<tokio::sync::Mutex<State>>,
    runtime: &std::sync::Arc<tokio::sync::Mutex<MatrixRuntime>>,
    event_bus: &std::sync::Arc<EventBus>,
    account_id: &str,
    subscription_id: &str,
) -> Result<(), RpcError> {
    let cursor = event_bus
        .latest_cursor(account_id)
        .await
        .unwrap_or_else(|| "cursor-0".to_string());
    let seq = cursor_to_seq(&cursor).unwrap_or(0);

    let account_snapshot = {
        let state = state.lock().await;
        state
            .accounts
            .iter()
            .find(|acct| acct.id == account_id)
            .cloned()
    };

    if let Some(account) = account_snapshot {
        let _ = send_event_push_raw(
            out_tx,
            subscription_id,
            account_id,
            "account.state",
            seq,
            &cursor,
            serde_json::json!({
                "account_id": account.id,
                "status": status_to_string(&account.status),
                "has_session": account.session.is_some(),
            }),
        );
    }

    let rooms = {
        let runtime = runtime.lock().await;
        if let Some(client) = runtime.get_client(account_id) {
            matrix::list_rooms(&client).await.map_err(internal_error)?
        } else {
            Vec::new()
        }
    };
    let _ = send_event_push_raw(
        out_tx,
        subscription_id,
        account_id,
        "room.list.snapshot",
        seq,
        &cursor,
        serde_json::json!({ "rooms": rooms }),
    );

    let verifications_handle = {
        let runtime = runtime.lock().await;
        runtime.verifications_handle()
    };
    let verifications: Vec<VerificationSnapshot> = {
        let guard = verifications_handle.lock().await;
        guard
            .get(account_id)
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default()
    };
    for verification in verifications {
        let flow_id = verification.flow_id;
        let user_id = verification.user_id;
        let device_id = verification.device_id;
        let stage = verification.stage;
        let _ = send_event_push_raw(
            out_tx,
            subscription_id,
            account_id,
            "matrix.verification.state",
            seq,
            &cursor,
            serde_json::json!({
                "flow_id": flow_id.clone(),
                "user_id": user_id.clone(),
                "device_id": device_id,
                "stage": stage,
            }),
        );
        if let Some(sas) = verification.sas {
            let _ = send_event_push_raw(
                out_tx,
                subscription_id,
                account_id,
                "matrix.verification.sas",
                seq,
                &cursor,
                serde_json::json!({
                    "flow_id": flow_id,
                    "user_id": user_id,
                    "supports_emoji": sas.get("supports_emoji").cloned().unwrap_or(serde_json::Value::Null),
                    "can_be_presented": sas.get("can_be_presented").cloned().unwrap_or(serde_json::Value::Null),
                    "emoji": sas.get("emoji").cloned().unwrap_or(serde_json::Value::Null),
                    "decimals": sas.get("decimals").cloned().unwrap_or(serde_json::Value::Null),
                }),
            );
        }
    }

    Ok(())
}

fn send_event_push(
    out_tx: &mpsc::UnboundedSender<String>,
    subscription_id: &str,
    event: &crate::events::ServerEvent,
) -> bool {
    send_event_push_raw(
        out_tx,
        subscription_id,
        &event.account_id,
        &event.event_type,
        event.seq,
        &event.cursor,
        event.data.clone(),
    )
}

fn send_event_push_raw(
    out_tx: &mpsc::UnboundedSender<String>,
    subscription_id: &str,
    account_id: &str,
    event_type: &str,
    seq: u64,
    cursor: &str,
    data: serde_json::Value,
) -> bool {
    let payload = serde_json::json!({
        "jsonrpc":"2.0",
        "method":"events.push",
        "params":{
            "subscription_id": subscription_id,
            "account_id": account_id,
            "seq": seq,
            "cursor": cursor,
            "type": event_type,
            "data": data,
        }
    });
    out_tx.send(payload.to_string()).is_ok()
}

fn cursor_to_seq(cursor: &str) -> Option<u64> {
    cursor.strip_prefix("cursor-")?.parse().ok()
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

async fn resolve_homeserver(homeserver: &str) -> Result<String, RpcError> {
    let client = matrix_sdk::Client::builder()
        .server_name_or_homeserver_url(homeserver)
        .build()
        .await
        .map_err(internal_error)?;
    Ok(client.homeserver().to_string())
}

fn extract_login_methods(value: &serde_json::Value) -> Vec<LoginMethod> {
    let flows = value
        .get("flows")
        .and_then(|flows| flows.as_array())
        .cloned()
        .unwrap_or_default();
    let mut methods = Vec::new();
    for flow in flows {
        let flow_type = flow.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if flow_type == "m.login.password" {
            if !methods.contains(&LoginMethod::Password) {
                methods.push(LoginMethod::Password);
            }
        } else if flow_type.starts_with("m.login.sso") || flow_type == "m.login.sso" {
            if !methods.contains(&LoginMethod::Sso) {
                methods.push(LoginMethod::Sso);
            }
        }
    }
    methods
}

fn resolve_user_id_param(client: &matrix_sdk::Client, value: &str) -> Result<String, RpcError> {
    if value == "me" || value == "self" {
        client
            .user_id()
            .map(|user_id| user_id.to_string())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "no session user id".to_string(),
            })
    } else {
        Ok(value.to_string())
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

fn status_to_string(status: &AccountStatus) -> &'static str {
    match status {
        AccountStatus::NeedsLogin => "needs_login",
        AccountStatus::LoginInProgress => "login_in_progress",
        AccountStatus::Ready => "ready",
    }
}

fn sas_to_json(sas: &matrix_sdk::encryption::verification::SasVerification) -> serde_json::Value {
    let mut response = serde_json::Map::new();
    response.insert(
        "supports_emoji".to_string(),
        serde_json::Value::Bool(sas.supports_emoji()),
    );
    response.insert(
        "can_be_presented".to_string(),
        serde_json::Value::Bool(sas.can_be_presented()),
    );
    response.insert("is_done".to_string(), serde_json::Value::Bool(sas.is_done()));
    response.insert(
        "is_cancelled".to_string(),
        serde_json::Value::Bool(sas.is_cancelled()),
    );
    if let Some(decimals) = sas.decimals() {
        response.insert(
            "decimals".to_string(),
            serde_json::json!([decimals.0, decimals.1, decimals.2]),
        );
    }
    if let Some(emojis) = sas.emoji() {
        let items: Vec<_> = emojis
            .iter()
            .map(|e| serde_json::json!({"symbol": e.symbol, "description": e.description}))
            .collect();
        response.insert("emoji".to_string(), serde_json::Value::Array(items));
    }
    serde_json::Value::Object(response)
}

async fn emit_sas_when_ready(
    event_bus: std::sync::Arc<EventBus>,
    verifications: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, VerificationSnapshot>>>,
    >,
    account_id: String,
    flow_id: String,
    user_id: String,
    sas: matrix_sdk::encryption::verification::SasVerification,
) {
    if sas.can_be_presented() {
        let sas_json = sas_to_json(&sas);
        if set_sas_snapshot(&verifications, &account_id, &flow_id, &user_id, &sas_json).await {
            emit_sas_events(&event_bus, &account_id, &flow_id, &user_id, &sas_json).await;
        }
        return;
    }

    let mut changes = sas.changes();
    while let Some(state) = changes.next().await {
        match state {
            SasState::KeysExchanged { .. } => {
                let sas_json = sas_to_json(&sas);
                if set_sas_snapshot(&verifications, &account_id, &flow_id, &user_id, &sas_json).await
                {
                    emit_sas_events(&event_bus, &account_id, &flow_id, &user_id, &sas_json).await;
                }
                break;
            }
            SasState::Cancelled(_) | SasState::Done { .. } => break,
            _ => {}
        }
    }
}

async fn set_sas_snapshot(
    verifications: &std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, VerificationSnapshot>>>,
    >,
    account_id: &str,
    flow_id: &str,
    user_id: &str,
    sas_json: &serde_json::Value,
) -> bool {
    let mut guard = verifications.lock().await;
    let entry = guard
        .entry(account_id.to_string())
        .or_insert_with(std::collections::HashMap::new)
        .entry(flow_id.to_string())
        .or_insert_with(|| VerificationSnapshot {
            flow_id: flow_id.to_string(),
            user_id: user_id.to_string(),
            device_id: None,
            stage: "sas".to_string(),
            sas: None,
        });
    if entry.sas.is_some() {
        false
    } else {
        entry.stage = "sas".to_string();
        entry.sas = Some(sas_json.clone());
        true
    }
}

async fn emit_sas_events(
    event_bus: &std::sync::Arc<EventBus>,
    account_id: &str,
    flow_id: &str,
    user_id: &str,
    sas_json: &serde_json::Value,
) {
    event_bus
        .emit(
            account_id,
            "matrix.verification.state",
            serde_json::json!({
                "flow_id": flow_id,
                "user_id": user_id,
                "stage": "sas",
            }),
        )
        .await;
    event_bus
        .emit(
            account_id,
            "matrix.verification.sas",
            serde_json::json!({
                "flow_id": flow_id,
                "user_id": user_id,
                "supports_emoji": sas_json.get("supports_emoji").cloned().unwrap_or(serde_json::Value::Null),
                "can_be_presented": sas_json.get("can_be_presented").cloned().unwrap_or(serde_json::Value::Null),
                "emoji": sas_json.get("emoji").cloned().unwrap_or(serde_json::Value::Null),
                "decimals": sas_json.get("decimals").cloned().unwrap_or(serde_json::Value::Null),
            }),
        )
        .await;
}
