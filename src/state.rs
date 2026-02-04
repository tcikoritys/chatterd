use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    Password,
    Sso,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    NeedsLogin,
    LoginInProgress,
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub homeserver: String,
    pub login_method: LoginMethod,
    pub status: AccountStatus,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    next_id: u64,
    accounts: Vec<Account>,
}

#[derive(Debug)]
pub struct State {
    pub next_id: u64,
    pub accounts: Vec<Account>,
    state_dir: PathBuf,
}

impl State {
    pub fn load(state_dir: PathBuf) -> Self {
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

    pub fn save(&self) -> anyhow::Result<()> {
        let path = self.state_dir.join("accounts.json");
        let persisted = PersistedState {
            next_id: self.next_id,
            accounts: self.accounts.clone(),
        };
        let contents = serde_json::to_string_pretty(&persisted)?;
        fs::write(&path, contents)?;
        Ok(())
    }
}
