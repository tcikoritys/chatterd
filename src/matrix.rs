use crate::state::MatrixSession;
use anyhow::Result;
use matrix_sdk::authentication::matrix::MatrixSession as SdkSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::store::RoomLoadSettings;
use matrix_sdk::{Client, Room};
use matrix_sdk::{
    encryption::verification::{SasVerification, VerificationRequest},
    ruma::{OwnedDeviceId, RoomId, UserId},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::task::JoinHandle;
use url::Url;

pub struct MatrixRuntime {
    clients: HashMap<String, Client>,
    sync_tasks: HashMap<String, JoinHandle<()>>,
}

impl MatrixRuntime {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            sync_tasks: HashMap::new(),
        }
    }

    pub fn get_client(&self, account_id: &str) -> Option<Client> {
        self.clients.get(account_id).cloned()
    }

    pub fn insert_client(&mut self, account_id: String, client: Client) {
        self.clients.insert(account_id, client);
    }

    pub fn stop_sync(&mut self, account_id: &str) {
        if let Some(handle) = self.sync_tasks.remove(account_id) {
            handle.abort();
        }
    }

    pub fn start_sync(&mut self, account_id: String, client: Client) {
        if self.sync_tasks.contains_key(&account_id) {
            return;
        }
        let handle = tokio::spawn(async move {
            let _ = client.sync(SyncSettings::default()).await;
        });
        self.sync_tasks.insert(account_id, handle);
    }
}

pub fn account_store_dir(state_dir: &Path, account_id: &str) -> PathBuf {
    state_dir.join("matrix").join(account_id)
}

pub async fn build_client(homeserver: &str, store_dir: &Path) -> Result<Client> {
    tokio::fs::create_dir_all(store_dir).await?;
    let homeserver = Url::parse(homeserver)?;
    let client = Client::builder()
        .homeserver_url(homeserver)
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
) -> Result<MessagesResponse> {
    let room_id = RoomId::parse(room_id)?;
    let room = client
        .get_room(&room_id)
        .ok_or_else(|| anyhow::anyhow!("unknown room"))?;

    let mut options = MessagesOptions::backward();
    options.limit = matrix_sdk::ruma::UInt::try_from(limit as u64)
        .map_err(|_| anyhow::anyhow!("invalid limit"))?;
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
