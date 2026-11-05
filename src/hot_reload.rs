use std::{
    env, fs,
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::handlers::state::State;

pub const HOT_RELOAD_ENV_VAR: &str = "TWT_HOT_RELOAD_FILE";

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinarySignature {
    inode: u64,
    mtime_sec: i64,
    mtime_nsec: i64,
    size: u64,
}

#[derive(Debug, Clone)]
pub struct BinaryWatcher {
    binary_path: PathBuf,
    initial_signature: Option<BinarySignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotReloadState {
    pub state: State,
    pub current_channel: String,
    pub connected_channel: Option<String>,
    pub scroll_offset: usize,
    pub chat_input_focused: bool,
    pub chat_input_draft: String,
    pub audio_enabled: bool,
    pub sidebar_visible: bool,
}

/// Resolves the on-disk binary path even after `cargo build` unlinks and replaces the running executable.
#[must_use]
pub fn current_binary_path() -> PathBuf {
    let Ok(exe) = env::current_exe() else {
        return PathBuf::from("twt");
    };
    let lossy = exe.to_string_lossy();
    if let Some(stripped) = lossy.strip_suffix(" (deleted)") {
        PathBuf::from(stripped)
    } else {
        exe
    }
}

fn read_signature(path: &Path) -> Option<(BinarySignature, Option<SystemTime>)> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let sig = BinarySignature {
        inode: meta.ino(),
        mtime_sec: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        size: meta.len(),
    };
    Some((sig, meta.modified().ok()))
}

impl Default for BinaryWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl BinaryWatcher {
    #[must_use]
    pub fn new() -> Self {
        let binary_path = current_binary_path();
        let initial_signature = read_signature(&binary_path).map(|(sig, _)| sig);
        Self {
            binary_path,
            initial_signature,
        }
    }

    /// Returns `true` when the on-disk executable has been replaced by a newly compiled binary
    /// and its modification timestamp has settled for at least 300ms (so linking is complete).
    #[must_use]
    pub fn is_binary_updated(&self) -> bool {
        let Some(ref initial) = self.initial_signature else {
            return false;
        };
        let Some((current, modified)) = read_signature(&self.binary_path) else {
            return false;
        };
        if &current == initial {
            return false;
        }
        // Ensure the linker has finished writing to the file before we exec into it
        if let Some(mtime) = modified {
            if mtime
                .elapsed()
                .is_ok_and(|age| age < Duration::from_millis(300))
            {
                return false;
            }
        }
        fs::File::open(&self.binary_path).is_ok()
    }
}

impl HotReloadState {
    pub fn save_to_disk(&self) -> Result<PathBuf> {
        let pid = std::process::id();
        let path = env::temp_dir().join(format!("twt-hot-reload-{pid}.json"));
        let payload = serde_json::to_string(self)?;
        fs::write(&path, payload)?;
        Ok(path)
    }

    #[must_use]
    pub fn load_from_env() -> Option<Self> {
        let path_str = env::var(HOT_RELOAD_ENV_VAR).ok()?;
        let path = PathBuf::from(path_str);
        let raw = fs::read_to_string(&path).ok()?;
        drop(fs::remove_file(&path));
        match serde_json::from_str::<Self>(&raw) {
            Ok(state) => {
                info!(
                    "Restored hot-reload state for channel #{} ({:?})",
                    state.current_channel, state.state
                );
                Some(state)
            }
            Err(err) => {
                warn!("Failed to deserialize hot-reload state: {err}");
                None
            }
        }
    }
}

/// Replaces the current process image in-place using `execv` (`CommandExt::exec`).
pub fn exec_new_binary(state_file: &Path) -> Result<()> {
    let exe = current_binary_path();
    let args: Vec<String> = env::args().skip(1).collect();
    info!(
        "Hot-reloading into {} with state file {}",
        exe.display(),
        state_file.display()
    );

    let err = Command::new(&exe)
        .args(&args)
        .env(HOT_RELOAD_ENV_VAR, state_file)
        .exec();

    Err(err.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hot_reload_state_roundtrip() {
        let state = HotReloadState {
            state: State::Normal,
            current_channel: "bowmandean".to_string(),
            connected_channel: Some("bowmandean".to_string()),
            scroll_offset: 3,
            chat_input_focused: true,
            chat_input_draft: "hello world".to_string(),
            audio_enabled: true,
            sidebar_visible: true,
        };

        let path = state.save_to_disk().unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        drop(fs::remove_file(&path));

        let restored: HotReloadState = serde_json::from_str(&raw).unwrap();
        assert_eq!(restored.state, State::Normal);
        assert_eq!(restored.current_channel, "bowmandean");
        assert_eq!(restored.scroll_offset, 3);
        assert!(restored.chat_input_focused);
        assert_eq!(restored.chat_input_draft, "hello world");
        assert!(restored.audio_enabled);
    }
}
