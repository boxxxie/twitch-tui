use std::{
    cell::RefCell,
    env,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{self, UnboundedReceiver};
use tracing::{error, info, warn};

use crate::config::SharedCoreConfig;

pub type SharedAudio = Rc<RefCell<StreamAudio>>;

const CANDIDATE_PLAYERS: &[(&str, &[&str])] = &[
    (
        "ffplay",
        &[
            "-nodisp",
            "-autoexit",
            "-loglevel",
            "quiet",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-probesize",
            "32768",
            "-analyzeduration",
            "0",
            "-i",
            "-",
        ],
    ),
    (
        "mpv",
        &[
            "--no-video",
            "--really-quiet",
            "--profile=low-latency",
            "--cache=no",
            "-",
        ],
    ),
    ("cvlc", &["--no-video", "--play-and-exit", "--quiet", "-"]),
    (
        "vlc",
        &[
            "--intf",
            "dummy",
            "--no-video",
            "--play-and-exit",
            "--quiet",
            "-",
        ],
    ),
];

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStatus {
    Muted,
    Connecting { channel: String, stage: String },
    Playing { channel: String },
    Offline { channel: String },
    Error { channel: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastSeverity {
    Info,
    Connecting,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct AudioToast {
    pub title: String,
    pub message: String,
    pub severity: ToastSeverity,
}

#[derive(Debug)]
enum AudioWorkerEvent {
    Stage(String),
    FirstByteReceived,
    StreamOffline,
    StreamError(String),
}

#[derive(Debug)]
pub struct StreamAudio {
    enabled: bool,
    current_channel: String,
    player_command: Vec<String>,
    status: AudioStatus,
    status_changed_at: Instant,
    connect_started_at: Option<Instant>,
    spinner_tick: usize,
    streamlink_process: Option<Child>,
    player_process: Option<Child>,
    stop_flag: Option<Arc<AtomicBool>>,
    event_rx: Option<UnboundedReceiver<AudioWorkerEvent>>,
}

fn command_exists_in_path(binary: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            let full_path = dir.join(binary);
            full_path.is_file()
        })
    })
}

#[must_use]
pub fn detect_default_audio_player() -> Vec<String> {
    for &(binary, args) in CANDIDATE_PLAYERS {
        if command_exists_in_path(binary) {
            let mut cmd = Vec::with_capacity(1 + args.len());
            cmd.push(binary.to_string());
            cmd.extend(args.iter().map(|&s| s.to_string()));
            return cmd;
        }
    }

    vec![
        "ffplay".to_string(),
        "-nodisp".to_string(),
        "-autoexit".to_string(),
        "-loglevel".to_string(),
        "quiet".to_string(),
        "-i".to_string(),
        "-".to_string(),
    ]
}

impl StreamAudio {
    #[must_use]
    pub fn new(config: &SharedCoreConfig) -> Self {
        let player_command = if config.frontend.audio_player.is_empty() {
            detect_default_audio_player()
        } else {
            config.frontend.audio_player.clone()
        };

        let enabled = config.frontend.audio;
        let status = if enabled {
            AudioStatus::Connecting {
                channel: config.twitch.channel.clone(),
                stage: "Waiting for channel...".to_string(),
            }
        } else {
            AudioStatus::Muted
        };

        Self {
            enabled,
            current_channel: config.twitch.channel.clone(),
            player_command,
            status,
            status_changed_at: Instant::now()
                .checked_sub(Duration::from_secs(10))
                .unwrap_or_else(Instant::now),
            connect_started_at: None,
            spinner_tick: 0,
            streamlink_process: None,
            player_process: None,
            stop_flag: None,
            event_rx: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn status(&self) -> &AudioStatus {
        &self.status
    }

    #[must_use]
    pub fn badge_label(&self) -> String {
        match &self.status {
            AudioStatus::Muted => "Audio: OFF".to_string(),
            AudioStatus::Connecting { .. } => {
                let frame = SPINNER_FRAMES
                    .get(self.spinner_tick % SPINNER_FRAMES.len())
                    .copied()
                    .unwrap_or("…");
                format!("Audio {frame}")
            }
            AudioStatus::Playing { .. } => "Audio: LIVE".to_string(),
            AudioStatus::Offline { .. } => "Audio: OFFLINE".to_string(),
            AudioStatus::Error { .. } => "Audio: ERR".to_string(),
        }
    }

    #[must_use]
    pub fn active_toast(&self) -> Option<AudioToast> {
        let elapsed = self.status_changed_at.elapsed();
        match &self.status {
            AudioStatus::Connecting { channel, stage } => {
                let frame = SPINNER_FRAMES
                    .get(self.spinner_tick % SPINNER_FRAMES.len())
                    .copied()
                    .unwrap_or("⠋");
                let secs = self
                    .connect_started_at
                    .map_or(0, |t| t.elapsed().as_secs());
                Some(AudioToast {
                    title: format!(" {frame} Audio · #{channel} ({secs}s) "),
                    message: stage.clone(),
                    severity: ToastSeverity::Connecting,
                })
            }
            AudioStatus::Playing { channel } if elapsed < Duration::from_millis(3500) => {
                Some(AudioToast {
                    title: format!(" 🔊 Audio Connected · #{channel} "),
                    message: "Live audio stream playing ([m] or click Audio to mute)".to_string(),
                    severity: ToastSeverity::Success,
                })
            }
            AudioStatus::Muted if elapsed < Duration::from_millis(2500) => Some(AudioToast {
                title: " 🔇 Audio Muted ".to_string(),
                message: "Stream audio stopped ([m] or click Audio to unmute)".to_string(),
                severity: ToastSeverity::Info,
            }),
            AudioStatus::Offline { channel } if elapsed < Duration::from_millis(4500) => {
                Some(AudioToast {
                    title: format!(" ○ Audio Unavailable · #{channel} "),
                    message: "Channel is currently offline on Twitch".to_string(),
                    severity: ToastSeverity::Warning,
                })
            }
            AudioStatus::Error { channel, reason } if elapsed < Duration::from_millis(5000) => {
                Some(AudioToast {
                    title: format!(" ⚠ Audio Error · #{channel} "),
                    message: reason.clone(),
                    severity: ToastSeverity::Error,
                })
            }
            _ => None,
        }
    }

    pub fn poll_status(&mut self) -> bool {
        let mut changed = false;

        if matches!(self.status, AudioStatus::Connecting { .. }) {
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
            changed = true;
        }

        let mut events = Vec::new();
        if let Some(rx) = &mut self.event_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
        }

        for ev in events {
            match ev {
                AudioWorkerEvent::Stage(stage_msg) => {
                    if let AudioStatus::Connecting { stage, .. } = &mut self.status {
                        if *stage != stage_msg {
                            *stage = stage_msg;
                            changed = true;
                        }
                    }
                }
                AudioWorkerEvent::FirstByteReceived => {
                    self.status = AudioStatus::Playing {
                        channel: self.current_channel.clone(),
                    };
                    self.status_changed_at = Instant::now();
                    changed = true;
                }
                AudioWorkerEvent::StreamOffline => {
                    self.status = AudioStatus::Offline {
                        channel: self.current_channel.clone(),
                    };
                    self.status_changed_at = Instant::now();
                    changed = true;
                }
                AudioWorkerEvent::StreamError(reason) => {
                    if !matches!(self.status, AudioStatus::Offline { .. }) {
                        self.status = AudioStatus::Error {
                            channel: self.current_channel.clone(),
                            reason,
                        };
                        self.status_changed_at = Instant::now();
                        changed = true;
                    }
                }
            }
        }

        // Check if streamlink exited unexpectedly before producing audio
        if let Some(child) = &mut self.streamlink_process {
            if let Ok(Some(exit_status)) = child.try_wait() {
                self.streamlink_process = None;
                if matches!(self.status, AudioStatus::Connecting { .. }) {
                    self.status = if exit_status.success() {
                        AudioStatus::Offline {
                            channel: self.current_channel.clone(),
                        }
                    } else {
                        AudioStatus::Offline {
                            channel: self.current_channel.clone(),
                        }
                    };
                    self.status_changed_at = Instant::now();
                    changed = true;
                }
            }
        }

        if self.active_toast().is_some() {
            changed = true;
        }

        changed
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        if self.enabled {
            let channel = self.current_channel.clone();
            self.play(&channel);
        } else {
            self.stop();
            self.status = AudioStatus::Muted;
            self.status_changed_at = Instant::now();
        }
    }

    pub fn set_channel(&mut self, channel: &str) {
        self.current_channel = channel.to_string();
        if self.enabled {
            self.play(channel);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Spawns streamlink, player, stdout proxy thread, and stderr stage parser thread"
    )]
    pub fn play(&mut self, channel: &str) {
        self.stop();
        self.current_channel = channel.to_string();

        if !self.enabled || channel.is_empty() {
            self.status = AudioStatus::Muted;
            return;
        }

        let Some((player_bin, player_args)) = self.player_command.split_first() else {
            warn!("No audio player command configured for streamlink audio.");
            self.status = AudioStatus::Error {
                channel: channel.to_string(),
                reason: "No audio player found (install ffplay or mpv)".to_string(),
            };
            self.status_changed_at = Instant::now();
            return;
        };

        self.status = AudioStatus::Connecting {
            channel: channel.to_string(),
            stage: format!("Launching streamlink for #{channel}..."),
        };
        self.status_changed_at = Instant::now();
        self.connect_started_at = Some(Instant::now());
        self.spinner_tick = 0;

        let stream_url = format!("https://twitch.tv/{channel}");

        let mut streamlink_child = match Command::new("streamlink")
            .args([
                "--twitch-low-latency",
                "--default-stream",
                "audio_only,worst",
                "--stdout",
                &stream_url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                error!("Failed to spawn streamlink for audio playback: {err}");
                self.status = AudioStatus::Error {
                    channel: channel.to_string(),
                    reason: format!("Failed to launch streamlink: {err}"),
                };
                self.status_changed_at = Instant::now();
                return;
            }
        };

        let Some(mut streamlink_stdout) = streamlink_child.stdout.take() else {
            error!("Failed to capture stdout pipe from streamlink process.");
            Self::terminate_child(&mut streamlink_child);
            return;
        };

        let streamlink_stderr = streamlink_child.stderr.take();

        let mut player_child = match Command::new(player_bin)
            .args(player_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                error!("Failed to spawn audio player '{player_bin}': {err}");
                Self::terminate_child(&mut streamlink_child);
                self.status = AudioStatus::Error {
                    channel: channel.to_string(),
                    reason: format!("Failed to spawn {player_bin}: {err}"),
                };
                self.status_changed_at = Instant::now();
                return;
            }
        };

        let Some(mut player_stdin) = player_child.stdin.take() else {
            Self::terminate_child(&mut player_child);
            Self::terminate_child(&mut streamlink_child);
            return;
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let stop_flag = Arc::new(AtomicBool::new(false));

        if let Some(stderr_pipe) = streamlink_stderr {
            let tx_err = tx.clone();
            let stop_err = stop_flag.clone();
            std::thread::Builder::new()
                .name("twt-audio-stderr".to_string())
                .spawn(move || {
                    let reader = BufReader::new(stderr_pipe);
                    for line in reader.lines().map_while(Result::ok) {
                        if stop_err.load(Ordering::Relaxed) {
                            break;
                        }
                        let trimmed = line.trim();
                        if trimmed.contains("No playable streams found") {
                            let _ = tx_err.send(AudioWorkerEvent::StreamOffline);
                        } else if trimmed.contains("Waiting for pre-roll ads")
                            || trimmed.contains("Filtering out segments")
                        {
                            let _ = tx_err.send(AudioWorkerEvent::Stage(
                                "Skipping Twitch pre-roll ads...".to_string(),
                            ));
                        } else if trimmed.contains("Opening stream:") {
                            let _ = tx_err.send(AudioWorkerEvent::Stage(
                                "Opening HLS audio_only stream...".to_string(),
                            ));
                        } else if trimmed.contains("Found matching plugin") {
                            let _ = tx_err.send(AudioWorkerEvent::Stage(
                                "Resolving Twitch HLS token...".to_string(),
                            ));
                        } else if trimmed.starts_with("error:") {
                            let _ = tx_err.send(AudioWorkerEvent::StreamError(trimmed.to_string()));
                        }
                    }
                })
                .ok();
        }

        let stop_pump = stop_flag.clone();
        std::thread::Builder::new()
            .name("twt-audio-pump".to_string())
            .spawn(move || {
                let mut buf = [0u8; 16384];
                let mut notified_first_byte = false;
                while !stop_pump.load(Ordering::Relaxed) {
                    match streamlink_stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if !notified_first_byte {
                                notified_first_byte = true;
                                let _ = tx.send(AudioWorkerEvent::FirstByteReceived);
                            }
                            if let Some(slice) = buf.get(..n) {
                                if player_stdin.write_all(slice).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            })
            .ok();

        info!("Started streamlink audio for channel '{channel}' via {player_bin}");
        self.streamlink_process = Some(streamlink_child);
        self.player_process = Some(player_child);
        self.stop_flag = Some(stop_flag);
        self.event_rx = Some(rx);
    }

    pub fn stop(&mut self) {
        if let Some(flag) = self.stop_flag.take() {
            flag.store(true, Ordering::Relaxed);
        }
        self.event_rx = None;
        if let Some(mut player) = self.player_process.take() {
            Self::terminate_child(&mut player);
        }
        if let Some(mut streamlink) = self.streamlink_process.take() {
            Self::terminate_child(&mut streamlink);
        }
    }

    fn terminate_child(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for StreamAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::CoreConfig;

    #[test]
    fn test_detect_default_audio_player_non_empty() {
        let player = detect_default_audio_player();
        assert_ne!(player, Vec::<String>::new());
    }

    #[test]
    fn test_stream_audio_initial_state_and_toggle() {
        let mut raw_config = CoreConfig::default();
        raw_config.frontend.audio = false;
        raw_config.twitch.channel = String::new();
        let config = Arc::new(raw_config);

        let mut audio = StreamAudio::new(&config);
        assert!(!audio.is_enabled());
        assert_eq!(*audio.status(), AudioStatus::Muted);

        audio.toggle();
        assert!(audio.is_enabled());

        audio.toggle();
        assert!(!audio.is_enabled());
        assert_eq!(*audio.status(), AudioStatus::Muted);
    }
}
