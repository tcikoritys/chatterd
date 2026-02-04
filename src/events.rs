use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(Clone, Debug)]
pub struct ServerEvent {
    pub account_id: String,
    pub seq: u64,
    pub cursor: String,
    pub event_type: String,
    pub data: Value,
}

#[derive(Debug)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
    history: Mutex<HashMap<String, VecDeque<ServerEvent>>>,
    seq: Mutex<HashMap<String, u64>>,
    max_history: usize,
}

impl EventBus {
    pub fn new(max_history: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            tx,
            history: Mutex::new(HashMap::new()),
            seq: Mutex::new(HashMap::new()),
            max_history,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    pub async fn latest_cursor(&self, account_id: &str) -> Option<String> {
        let seq = self.seq.lock().await;
        seq.get(account_id).map(|value| format!("cursor-{}", value))
    }

    pub async fn emit(
        &self,
        account_id: &str,
        event_type: &str,
        data: Value,
    ) -> ServerEvent {
        let seq_value = {
            let mut seq = self.seq.lock().await;
            let entry = seq.entry(account_id.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        let cursor = format!("cursor-{}", seq_value);
        let event = ServerEvent {
            account_id: account_id.to_string(),
            seq: seq_value,
            cursor: cursor.clone(),
            event_type: event_type.to_string(),
            data,
        };

        {
            let mut history = self.history.lock().await;
            let entries = history
                .entry(account_id.to_string())
                .or_insert_with(VecDeque::new);
            entries.push_back(event.clone());
            while entries.len() > self.max_history {
                entries.pop_front();
            }
        }

        let _ = self.tx.send(event.clone());
        event
    }

    pub async fn replay_since(
        &self,
        account_id: &str,
        since: &str,
    ) -> Option<Vec<ServerEvent>> {
        let history = self.history.lock().await;
        let entries = history.get(account_id)?;
        let mut found = false;
        let mut replay = Vec::new();
        for entry in entries {
            if found {
                replay.push(entry.clone());
            } else if entry.cursor == since {
                found = true;
            }
        }
        if found {
            Some(replay)
        } else {
            None
        }
    }
}
