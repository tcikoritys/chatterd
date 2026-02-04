use crate::events::EventBus;
use crate::state::MatrixSession;
use anyhow::Result;
use matrix_sdk::authentication::matrix::MatrixSession as SdkSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::store::RoomLoadSettings;
use matrix_sdk::{Client, Room};
use matrix_sdk::{
    encryption::verification::{SasVerification, VerificationRequest},
    ruma::{
        events::{
            room::message::RoomMessageEventContent,
            AnySyncTimelineEvent,
            AnyToDeviceEvent,
        },
        serde::Raw,
        OwnedDeviceId,
        OwnedTransactionId,
        RoomId,
        UserId,
    },
    deserialized_responses::EncryptionInfo,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use matrix_sdk::encryption::verification::SasState;
use matrix_sdk::stream::StreamExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub struct MatrixRuntime {
    clients: HashMap<String, Client>,
    sync_tasks: HashMap<String, JoinHandle<()>>,
    event_bus: Arc<EventBus>,
    verifications: Arc<Mutex<HashMap<String, HashMap<String, VerificationSnapshot>>>>,
}

impl MatrixRuntime {
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            clients: HashMap::new(),
            sync_tasks: HashMap::new(),
            event_bus,
            verifications: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_client(&self, account_id: &str) -> Option<Client> {
        self.clients.get(account_id).cloned()
    }

    pub fn insert_client(&mut self, account_id: String, client: Client) {
        self.clients.insert(account_id, client);
    }

    pub fn start_sync(&mut self, account_id: String, client: Client) {
        if self.sync_tasks.contains_key(&account_id) {
            return;
        }
        let bus = self.event_bus.clone();
        let account = account_id.clone();
        let handler_bus = bus.clone();
        let handler_account = account.clone();
        client.add_event_handler(move |raw: Raw<AnySyncTimelineEvent>, room: Room| {
            let bus = handler_bus.clone();
            let account = handler_account.clone();
            async move {
                if let Ok(event_value) = raw_to_value(&raw) {
                    let data = serde_json::json!({
                        "room_id": room.room_id().to_string(),
                        "event": event_value,
                    });
                    bus.emit(&account, "room.message", data).await;
                }
            }
        });
        let to_device_bus = bus.clone();
        let to_device_account = account.clone();
        let to_device_verifications = self.verifications.clone();
        let to_device_client = client.clone();
        client.add_event_handler(
            move |raw: Raw<AnyToDeviceEvent>, _info: Option<EncryptionInfo>| {
                let bus = to_device_bus.clone();
                let account = to_device_account.clone();
                let verifications = to_device_verifications.clone();
                let client = to_device_client.clone();
                async move {
                    let Ok(Some(event_type)) = raw.get_field::<String>("type") else {
                        return;
                    };
                    let sender = raw.get_field::<String>("sender").ok().flatten();
                    let content = raw
                        .get_field::<serde_json::Value>("content")
                        .ok()
                        .flatten();
                    let transaction_id = content
                        .as_ref()
                        .and_then(|value| value.get("transaction_id"))
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                    let sender = match sender {
                        Some(value) => value,
                        None => return,
                    };

                    if event_type == "m.key.verification.request" {
                        let (flow_id, device_id) = match content {
                            Some(content) => {
                                let flow_id = content
                                    .get("transaction_id")
                                    .and_then(|v| v.as_str())
                                    .map(|v| v.to_string());
                                let device_id = content
                                    .get("from_device")
                                    .and_then(|v| v.as_str())
                                    .map(|v| v.to_string());
                                (flow_id, device_id)
                            }
                            None => (None, None),
                        };
                        let Some(flow_id) = flow_id else {
                            return;
                        };
                        let snapshot = VerificationSnapshot {
                            flow_id: flow_id.clone(),
                            user_id: sender.to_string(),
                            device_id: device_id.clone(),
                            stage: "requested".to_string(),
                            sas: None,
                        };
                        {
                            let mut guard = verifications.lock().await;
                            guard
                                .entry(account.clone())
                                .or_insert_with(HashMap::new)
                                .insert(snapshot.flow_id.clone(), snapshot.clone());
                        }
                        bus.emit(
                            &account,
                            "matrix.verification.state",
                            serde_json::json!({
                                "flow_id": snapshot.flow_id,
                                "user_id": snapshot.user_id,
                                "device_id": snapshot.device_id,
                                "stage": "requested",
                            }),
                        )
                        .await;
                        return;
                    }

                    if matches!(
                        event_type.as_str(),
                        "m.key.verification.ready"
                            | "m.key.verification.start"
                            | "m.key.verification.accept"
                            | "m.key.verification.key"
                            | "m.key.verification.mac"
                    ) {
                        let Some(flow_id) = transaction_id else {
                            return;
                        };
                        let user_id = match UserId::parse(&sender) {
                            Ok(user_id) => user_id,
                            Err(_) => return,
                        };
                        let sas = match client
                            .encryption()
                            .get_verification(&user_id, &flow_id)
                            .await
                        {
                            Some(verification) => verification.sas(),
                            None => None,
                        };
                        if let Some(sas) = sas {
                            let _ = sas.accept().await;
                            let bus = bus.clone();
                            let verifications = verifications.clone();
                            let account = account.clone();
                            let flow_id = flow_id.clone();
                            let sender = sender.clone();
                            tokio::spawn(async move {
                                emit_sas_when_ready(
                                    bus,
                                    verifications,
                                    account,
                                    flow_id,
                                    sender,
                                    sas,
                                )
                                .await;
                            });
                        }
                        return;
                    }

                    if event_type == "m.key.verification.done" {
                        let Some(flow_id) = transaction_id else {
                            return;
                        };
                        let removed = {
                            let mut guard = verifications.lock().await;
                            guard
                                .get_mut(&account)
                                .and_then(|entries| entries.remove(&flow_id))
                                .is_some()
                        };
                        if removed {
                            bus.emit(
                                &account,
                                "matrix.verification.done",
                                serde_json::json!({
                                    "flow_id": flow_id,
                                    "user_id": sender,
                                    "reason": "verified",
                                }),
                            )
                            .await;
                        }
                        return;
                    }

                    if event_type == "m.key.verification.cancel" {
                        let Some(flow_id) = transaction_id else {
                            return;
                        };
                        let reason = content
                            .as_ref()
                            .and_then(|value| value.get("reason"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("cancelled")
                            .to_string();
                        let removed = {
                            let mut guard = verifications.lock().await;
                            guard
                                .get_mut(&account)
                                .and_then(|entries| entries.remove(&flow_id))
                                .is_some()
                        };
                        if removed {
                            bus.emit(
                                &account,
                                "matrix.verification.cancelled",
                                serde_json::json!({
                                    "flow_id": flow_id,
                                    "user_id": sender,
                                    "reason": reason,
                                }),
                            )
                            .await;
                        }
                    }
                }
            },
        );
        let sync_bus = bus.clone();
        let sync_account = account_id.clone();
        let handle = tokio::spawn(async move {
            sync_bus
                .emit(
                    &sync_account,
                    "matrix.sync.state",
                    serde_json::json!({"state":"syncing"}),
                )
                .await;
            match client.sync(SyncSettings::default()).await {
                Ok(_) => {
                    sync_bus
                        .emit(
                            &sync_account,
                            "matrix.sync.state",
                            serde_json::json!({"state":"idle"}),
                        )
                        .await;
                }
                Err(err) => {
                    sync_bus
                        .emit(
                            &sync_account,
                            "matrix.sync.state",
                            serde_json::json!({"state":"error","error": err.to_string()}),
                        )
                        .await;
                }
            }
        });
        self.sync_tasks.insert(account_id, handle);
    }

    pub fn verifications_handle(
        &self,
    ) -> Arc<Mutex<HashMap<String, HashMap<String, VerificationSnapshot>>>> {
        self.verifications.clone()
    }
}

fn sas_to_json(sas: &SasVerification) -> serde_json::Value {
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
    bus: Arc<EventBus>,
    verifications: Arc<Mutex<HashMap<String, HashMap<String, VerificationSnapshot>>>>,
    account: String,
    flow_id: String,
    sender: String,
    sas: SasVerification,
) {
    if sas.can_be_presented() {
        let sas_json = sas_to_json(&sas);
        if set_sas_snapshot(&verifications, &account, &flow_id, &sender, &sas_json).await {
            emit_sas_events(&bus, &account, &flow_id, &sender, &sas_json).await;
        }
        return;
    }

    let mut changes = sas.changes();
    while let Some(state) = changes.next().await {
        match state {
            SasState::KeysExchanged { .. } => {
                let sas_json = sas_to_json(&sas);
                if set_sas_snapshot(&verifications, &account, &flow_id, &sender, &sas_json).await {
                    emit_sas_events(&bus, &account, &flow_id, &sender, &sas_json).await;
                }
                break;
            }
            SasState::Cancelled(_) | SasState::Done { .. } => {
                break;
            }
            _ => {}
        }
    }
}

async fn set_sas_snapshot(
    verifications: &Arc<Mutex<HashMap<String, HashMap<String, VerificationSnapshot>>>>,
    account: &str,
    flow_id: &str,
    sender: &str,
    sas_json: &serde_json::Value,
) -> bool {
    let mut guard = verifications.lock().await;
    let entry = guard
        .entry(account.to_string())
        .or_insert_with(HashMap::new)
        .entry(flow_id.to_string())
        .or_insert_with(|| VerificationSnapshot {
            flow_id: flow_id.to_string(),
            user_id: sender.to_string(),
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
    bus: &Arc<EventBus>,
    account: &str,
    flow_id: &str,
    sender: &str,
    sas_json: &serde_json::Value,
) {
    bus.emit(
        account,
        "matrix.verification.state",
        serde_json::json!({
            "flow_id": flow_id,
            "user_id": sender,
            "stage": "sas",
        }),
    )
    .await;
    bus.emit(
        account,
        "matrix.verification.sas",
        serde_json::json!({
            "flow_id": flow_id,
            "user_id": sender,
            "supports_emoji": sas_json.get("supports_emoji").cloned().unwrap_or(serde_json::Value::Null),
            "can_be_presented": sas_json.get("can_be_presented").cloned().unwrap_or(serde_json::Value::Null),
            "emoji": sas_json.get("emoji").cloned().unwrap_or(serde_json::Value::Null),
            "decimals": sas_json.get("decimals").cloned().unwrap_or(serde_json::Value::Null),
        }),
    )
    .await;
}

pub fn account_store_dir(state_dir: &Path, account_id: &str) -> PathBuf {
    state_dir.join("matrix").join(account_id)
}

pub async fn build_client(homeserver: &str, store_dir: &Path) -> Result<Client> {
    tokio::fs::create_dir_all(store_dir).await?;
    let client = Client::builder()
        .server_name_or_homeserver_url(homeserver)
        .sqlite_store(store_dir.join("store.db"), None)
        .build()
        .await?;
    Ok(client)
}

pub fn session_from_sdk(session: SdkSession) -> MatrixSession {
    MatrixSession {
        user_id: session.meta.user_id.to_string(),
        device_id: session.meta.device_id.to_string(),
        access_token: session.tokens.access_token,
        refresh_token: session.tokens.refresh_token,
    }
}

pub fn sdk_session_from_stored(session: &MatrixSession) -> Result<SdkSession> {
    let user_id = UserId::parse(&session.user_id)?;
    let device_id = OwnedDeviceId::try_from(session.device_id.as_str())?;
    Ok(SdkSession {
        meta: matrix_sdk::SessionMeta {
            user_id,
            device_id,
        },
        tokens: matrix_sdk::SessionTokens {
            access_token: session.access_token.clone(),
            refresh_token: session.refresh_token.clone(),
        },
    })
}

pub async fn restore_session(
    client: &Client,
    session: SdkSession,
) -> Result<()> {
    client
        .matrix_auth()
        .restore_session(session, RoomLoadSettings::default())
        .await?;
    Ok(())
}

pub async fn login_password(
    client: &Client,
    username: &str,
    password: &str,
    device_name: Option<String>,
) -> Result<MatrixSession> {
    let mut builder = client.matrix_auth().login_username(username, password);
    if let Some(name) = device_name {
        builder = builder.initial_device_display_name(&name);
    }
    let _ = builder.send().await?;
    let session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| anyhow::anyhow!("missing session after login"))?;
    Ok(session_from_sdk(session))
}

pub async fn login_token(
    client: &Client,
    token: &str,
    device_name: Option<String>,
) -> Result<MatrixSession> {
    let mut builder = client.matrix_auth().login_token(token);
    if let Some(name) = device_name {
        builder = builder.initial_device_display_name(&name);
    }
    let _ = builder.send().await?;
    let session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| anyhow::anyhow!("missing session after login"))?;
    Ok(session_from_sdk(session))
}

pub async fn list_rooms(client: &Client) -> Result<Vec<RoomInfo>> {
    let mut rooms = Vec::new();
    for room in client.rooms() {
        rooms.push(RoomInfo::from_room(&room).await?);
    }
    Ok(rooms)
}

pub async fn fetch_messages(
    client: &Client,
    room_id: &str,
    limit: usize,
    from: Option<String>,
) -> Result<MessagesResponse> {
    let room_id = RoomId::parse(room_id)?;
    let room = client
        .get_room(&room_id)
        .ok_or_else(|| anyhow::anyhow!("unknown room"))?;

    let mut options = MessagesOptions::backward();
    options.limit = matrix_sdk::ruma::UInt::try_from(limit as u64)
        .map_err(|_| anyhow::anyhow!("invalid limit"))?;
    options.from = from;
    let messages = room.messages(options).await?;
    Ok(MessagesResponse {
        start: messages.start,
        end: messages.end,
        chunk: messages
            .chunk
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub async fn send_message_text(
    client: &Client,
    room_id: &str,
    body: &str,
    txn_id: Option<String>,
) -> Result<String> {
    let room_id = RoomId::parse(room_id)?;
    let room = client
        .get_room(&room_id)
        .ok_or_else(|| anyhow::anyhow!("unknown room"))?;
    let content = RoomMessageEventContent::text_plain(body);
    let mut send = room.send(content);
    if let Some(txn_id) = txn_id {
        send = send.with_transaction_id(OwnedTransactionId::from(txn_id));
    }
    let response = send.await?;
    Ok(response.event_id.to_string())
}

pub async fn get_verification_request(
    client: &Client,
    user_id: &str,
    flow_id: &str,
) -> Result<Option<VerificationRequest>> {
    let user_id = UserId::parse(user_id)?;
    Ok(client
        .encryption()
        .get_verification_request(&user_id, flow_id)
        .await)
}

pub async fn get_sas_verification(
    client: &Client,
    user_id: &str,
    flow_id: &str,
) -> Result<Option<SasVerification>> {
    let user_id = UserId::parse(user_id)?;
    Ok(client
        .encryption()
        .get_verification(&user_id, flow_id)
        .await
        .and_then(|v| v.sas()))
}

#[derive(Debug, serde::Serialize)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: Option<String>,
    pub is_encrypted: bool,
    pub is_direct: bool,
}

impl RoomInfo {
    async fn from_room(room: &Room) -> Result<Self> {
        let name = room.display_name().await.ok().map(|n| n.to_string());
        let is_direct = room.is_direct().await.ok().unwrap_or(false);
        let is_encrypted = room
            .latest_encryption_state()
            .await
            .map(|state| state.is_encrypted())
            .unwrap_or(false);
        Ok(Self {
            room_id: room.room_id().to_string(),
            name,
            is_encrypted,
            is_direct,
        })
    }
}

#[derive(Debug, serde::Serialize)]
pub struct MessagesResponse {
    pub start: String,
    pub end: Option<String>,
    pub chunk: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct VerificationSnapshot {
    pub flow_id: String,
    pub user_id: String,
    pub device_id: Option<String>,
    pub stage: String,
    pub sas: Option<serde_json::Value>,
}

fn raw_to_value(raw: &Raw<AnySyncTimelineEvent>) -> Result<serde_json::Value> {
    let json = raw.json().get();
    let value = serde_json::from_str(json)?;
    Ok(value)
}
