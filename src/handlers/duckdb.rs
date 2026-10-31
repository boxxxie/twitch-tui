use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

use chrono::{DateTime, Local};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::handlers::data::MessageData;

pub type SharedDuckDb = Rc<RefCell<DuckDbStore>>;

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

const SCHEMA_SQL: &str = r"
CREATE TABLE IF NOT EXISTS messages (
    id VARCHAR PRIMARY KEY,
    channel VARCHAR,
    time_sent VARCHAR,
    epoch_ms BIGINT,
    author VARCHAR,
    user_id VARCHAR,
    system BOOLEAN,
    highlight BOOLEAN,
    badges VARCHAR,
    payload VARCHAR,
    raw_json VARCHAR
);
CREATE TABLE IF NOT EXISTS events (
    id VARCHAR PRIMARY KEY,
    channel VARCHAR,
    event_type VARCHAR,
    time_sent VARCHAR,
    epoch_ms BIGINT,
    author VARCHAR,
    user_id VARCHAR,
    message_id VARCHAR,
    payload VARCHAR
);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMessage {
    pub id: String,
    pub channel: String,
    pub time_sent: DateTime<Local>,
    pub author: String,
    pub user_id: Option<String>,
    pub system: bool,
    pub payload: String,
    pub message_id: Option<String>,
    pub highlight: bool,
    pub badges: Option<String>,
}

impl PersistedMessage {
    #[must_use]
    pub fn from_message_data(channel: &str, msg: &MessageData) -> Self {
        let seq = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = msg.message_id.clone().unwrap_or_else(|| {
            format!(
                "{}-{}-{}",
                channel,
                msg.time_sent.timestamp_millis(),
                seq
            )
        });

        Self {
            id,
            channel: channel.to_lowercase(),
            time_sent: msg.time_sent,
            author: msg.author.clone(),
            user_id: msg.user_id.clone(),
            system: msg.system,
            payload: msg.payload.clone(),
            message_id: msg.message_id.clone(),
            highlight: msg.highlight,
            badges: msg.badges.clone(),
        }
    }

    #[must_use]
    pub fn into_message_data(self) -> MessageData {
        MessageData {
            time_sent: self.time_sent,
            author: self.author,
            user_id: self.user_id,
            system: self.system,
            payload: self.payload,
            emotes: Vec::new(),
            message_id: self.message_id,
            highlight: self.highlight,
            badges: self.badges,
        }
    }
}

#[derive(Debug)]
enum SqlWorkerMsg {
    Execute(String),
    Shutdown(mpsc::SyncSender<()>),
}

#[derive(Debug)]
pub struct DuckDbStore {
    db_path: PathBuf,
    duckdb_bin: PathBuf,
    messages_by_channel: HashMap<String, VecDeque<MessageData>>,
    sql_tx: Option<Sender<SqlWorkerMsg>>,
}

#[must_use]
pub fn resolve_duckdb_binary() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        let home_duckdb = base.home_dir().join(".local").join("bin").join("duckdb");
        if home_duckdb.exists() {
            return home_duckdb;
        }
    }
    PathBuf::from("duckdb")
}

#[must_use]
pub fn default_duckdb_path() -> PathBuf {
    BaseDirs::new().map_or_else(
        || PathBuf::from("/tmp/twt.duckdb"),
        |base| base.data_local_dir().join("twt").join("twt.duckdb"),
    )
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''").replace(['\r', '\n'], " ")
}

fn sql_opt_str(opt: Option<&str>) -> String {
    opt.map_or_else(|| "NULL".to_string(), |s| format!("'{}'", sql_escape(s)))
}

impl Default for DuckDbStore {
    fn default() -> Self {
        Self::new(default_duckdb_path())
    }
}

impl DuckDbStore {
    #[must_use]
    pub fn new(db_path: PathBuf) -> Self {
        if let Some(parent) = db_path.parent() {
            drop(std::fs::create_dir_all(parent));
        }

        let duckdb_bin = resolve_duckdb_binary();

        let mut store = Self {
            db_path,
            duckdb_bin,
            messages_by_channel: HashMap::new(),
            sql_tx: None,
        };

        if store.db_path.exists() {
            store.hydrate_from_disk();
        } else {
            drop(
                Command::new(&store.duckdb_bin)
                    .arg("-dark-mode")
                    .arg("-batch")
                    .arg(&store.db_path)
                    .arg("-c")
                    .arg(SCHEMA_SQL)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status(),
            );
        }

        store.start_persistent_worker();
        store
    }

    fn hydrate_from_disk(&mut self) {
        let output = Command::new(&self.duckdb_bin)
            .arg("-dark-mode")
            .arg("-readonly")
            .arg("-json")
            .arg(&self.db_path)
            .arg("-c")
            .arg("SELECT raw_json FROM messages ORDER BY epoch_ms ASC;")
            .output();

        let Ok(out) = output else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&out.stdout) else {
            return;
        };

        for row in rows {
            let Some(raw_str) = row.get("raw_json").and_then(Value::as_str) else {
                continue;
            };
            let Ok(persisted) = serde_json::from_str::<PersistedMessage>(raw_str) else {
                continue;
            };
            let channel = persisted.channel.to_lowercase();
            let msg = persisted.into_message_data();
            let queue = self.messages_by_channel.entry(channel).or_default();
            queue.push_front(msg);
        }
    }

    fn start_persistent_worker(&mut self) {
        let Ok(mut child): Result<Child, _> = Command::new(&self.duckdb_bin)
            .arg("-dark-mode")
            .arg("-batch")
            .arg(&self.db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            warn!("Failed to spawn persistent DuckDB worker for twitch-tui");
            return;
        };

        let Some(mut stdin): Option<ChildStdin> = child.stdin.take() else {
            return;
        };

        if let Some(stdout) = child.stdout.take() {
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut sink = String::new();
                while let Ok(n) = reader.read_line(&mut sink) {
                    if n == 0 {
                        break;
                    }
                    sink.clear();
                }
            });
        }

        drop(writeln!(stdin, "{SCHEMA_SQL}"));
        drop(stdin.flush());

        let (sql_tx, sql_rx) = mpsc::channel::<SqlWorkerMsg>();
        let db_path_display = self.db_path.display().to_string();
        info!("Initialized DuckDB message & event store at {db_path_display}");

        thread::spawn(move || {
            let mut ack_on_exit = None;
            while let Ok(msg) = sql_rx.recv() {
                match msg {
                    SqlWorkerMsg::Execute(sql) => {
                        if writeln!(stdin, "{sql}").is_err() {
                            break;
                        }
                        drop(stdin.flush());
                    }
                    SqlWorkerMsg::Shutdown(ack) => {
                        drop(writeln!(stdin, "CHECKPOINT;"));
                        drop(stdin.flush());
                        ack_on_exit = Some(ack);
                        break;
                    }
                }
            }
            drop(stdin);
            drop(child.wait());
            if let Some(ack) = ack_on_exit {
                let _ = ack.send(());
            }
        });

        self.sql_tx = Some(sql_tx);
    }

    fn execute_async(&self, sql: String) {
        if let Some(tx) = &self.sql_tx {
            let _ = tx.send(SqlWorkerMsg::Execute(sql));
        }
    }

    pub fn record_message(&mut self, channel: &str, msg: &MessageData, max_messages: usize) {
        let clean_channel = channel.to_lowercase();
        let queue = self
            .messages_by_channel
            .entry(clean_channel.clone())
            .or_default();
        queue.push_front(msg.clone());
        if queue.len() > max_messages {
            queue.truncate(max_messages);
        }

        let persisted = PersistedMessage::from_message_data(&clean_channel, msg);
        let raw_json = serde_json::to_string(&persisted).unwrap_or_default();
        let epoch_ms = persisted.time_sent.timestamp_millis();
        let time_iso = persisted.time_sent.to_rfc3339();
        let event_type = if persisted.system {
            "system_message"
        } else {
            "chat_message"
        };

        let sql = format!(
            "INSERT OR REPLACE INTO messages (id, channel, time_sent, epoch_ms, author, user_id, system, highlight, badges, payload, raw_json) VALUES ('{}', '{}', '{}', {}, '{}', {}, {}, {}, {}, '{}', '{}');\nINSERT OR REPLACE INTO events (id, channel, event_type, time_sent, epoch_ms, author, user_id, message_id, payload) VALUES ('{}', '{}', '{}', '{}', {}, '{}', {}, {}, '{}');",
            sql_escape(&persisted.id),
            sql_escape(&persisted.channel),
            sql_escape(&time_iso),
            epoch_ms,
            sql_escape(&persisted.author),
            sql_opt_str(persisted.user_id.as_deref()),
            if persisted.system { "TRUE" } else { "FALSE" },
            if persisted.highlight { "TRUE" } else { "FALSE" },
            sql_opt_str(persisted.badges.as_deref()),
            sql_escape(&persisted.payload),
            sql_escape(&raw_json),
            sql_escape(&persisted.id),
            sql_escape(&persisted.channel),
            event_type,
            sql_escape(&time_iso),
            epoch_ms,
            sql_escape(&persisted.author),
            sql_opt_str(persisted.user_id.as_deref()),
            sql_opt_str(persisted.message_id.as_deref()),
            sql_escape(&persisted.payload),
        );

        self.execute_async(sql);
    }

    pub fn record_event(
        &self,
        channel: &str,
        event_type: &str,
        author: Option<&str>,
        user_id: Option<&str>,
        message_id: Option<&str>,
        payload: &str,
    ) {
        let now = Local::now();
        let epoch_ms = now.timestamp_millis();
        let time_iso = now.to_rfc3339();
        let seq = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("{channel}-{event_type}-{epoch_ms}-{seq}");

        let sql = format!(
            "INSERT OR REPLACE INTO events (id, channel, event_type, time_sent, epoch_ms, author, user_id, message_id, payload) VALUES ('{}', '{}', '{}', '{}', {}, {}, {}, {}, '{}');",
            sql_escape(&id),
            sql_escape(&channel.to_lowercase()),
            sql_escape(event_type),
            sql_escape(&time_iso),
            epoch_ms,
            sql_opt_str(author),
            sql_opt_str(user_id),
            sql_opt_str(message_id),
            sql_escape(payload),
        );

        self.execute_async(sql);
    }

    /// Returns up to `limit` most recent messages for `channel`
    /// ordered newest-first (`VecDeque` front = newest, matching `App::messages`).
    #[must_use]
    pub fn load_channel_messages(&self, channel: &str, limit: usize) -> VecDeque<MessageData> {
        let clean_channel = channel.to_lowercase();
        self.messages_by_channel
            .get(&clean_channel)
            .map_or_else(VecDeque::new, |q| q.iter().take(limit).cloned().collect())
    }

    #[must_use]
    pub fn known_channels(&self) -> Vec<String> {
        self.messages_by_channel.keys().cloned().collect()
    }

    pub fn shutdown(&mut self) {
        if let Some(tx) = self.sql_tx.take() {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(SqlWorkerMsg::Shutdown(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(Duration::from_secs(2));
            }
        }
    }
}

impl Drop for DuckDbStore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[expect(dead_code, reason = "Helper constructor for tests")]
#[must_use]
pub fn store_with_custom_path(path: &Path) -> DuckDbStore {
    DuckDbStore::new(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persisted_message_roundtrip() {
        let msg = MessageData {
            time_sent: Local::now(),
            author: "boxxy_queen_of_b".to_string(),
            user_id: Some("47685904".to_string()),
            system: false,
            payload: "hello duckdb".to_string(),
            emotes: vec![],
            message_id: Some("msg-123".to_string()),
            highlight: false,
            badges: None,
        };

        let persisted = PersistedMessage::from_message_data("BowmanDean", &msg);
        assert_eq!(persisted.channel, "bowmandean");
        assert_eq!(persisted.id, "msg-123");

        let restored = persisted.into_message_data();
        assert_eq!(restored.author, "boxxy_queen_of_b");
        assert_eq!(restored.payload, "hello duckdb");
    }
}
