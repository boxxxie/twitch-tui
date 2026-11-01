use std::fmt::Display;

use color_eyre::{Result, eyre::Context};
use tokio::sync::mpsc::Sender;
use tui::{Frame, layout::Rect};

use crate::{
    config::SharedCoreConfig,
    emotes::SharedEmotes,
    events::{Event, InternalEvent, Key, TwitchAction, TwitchEvent},
    handlers::storage::SharedStorage,
    ui::{
        components::{Component, EmotePickerWidget, utils::InputWidget},
        statics::{SUPPORTED_COMMANDS, TWITCH_MESSAGE_LIMIT},
    },
    utils::text::first_similarity,
};

const MAX_SENT_HISTORY: usize = 50;

pub struct ChatInputWidget {
    config: SharedCoreConfig,
    event_tx: Sender<Event>,
    storage: SharedStorage,
    input: InputWidget<SharedStorage>,
    emote_picker: EmotePickerWidget,
    sent_history: Vec<String>,
    history_idx: Option<usize>,
    draft: String,
}

impl ChatInputWidget {
    pub fn new(
        config: SharedCoreConfig,
        event_tx: Sender<Event>,
        storage: SharedStorage,
        emotes: SharedEmotes,
    ) -> Self {
        let input_validator = Box::new(|_, s: String| -> bool {
            {
                if !s.is_empty() && s.len() < TWITCH_MESSAGE_LIMIT {
                    s.strip_prefix('/').is_none_or(|command| {
                        let command = command.split(' ').next().unwrap_or("");
                        SUPPORTED_COMMANDS.contains(&command)
                    })
                } else {
                    false
                }
            }
        });

        // User should be known of how close they are to the message length limit.
        let visual_indicator =
            Box::new(|s: String| -> String { format!("{} / {}", s.len(), TWITCH_MESSAGE_LIMIT) });

        let input_suggester = Box::new(|storage: SharedStorage, s: String| -> Option<String> {
            if let Some(command) = s.strip_prefix('/') {
                if !command.contains(' ') {
                    let possible_suggestion = first_similarity(
                        &SUPPORTED_COMMANDS
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<String>>(),
                        command,
                    );
                    return possible_suggestion.map(|sug| format!("/{sug} "));
                }
            }

            // Support @mention completion both at the start of the message and after any space
            let at_idx = s.rfind('@')?;
            let before_at = &s[..at_idx];
            if !before_at.is_empty() && !before_at.ends_with(' ') {
                return None;
            }
            let partial = &s[at_idx + 1..];
            if partial.is_empty() || partial.contains(' ') {
                return None;
            }

            let storage_ref = storage.borrow();
            let mentions = storage_ref.get("mentions").unwrap_or_default();
            let chatters = storage_ref.get("chatters").unwrap_or_default();
            let channels = storage_ref.get("channels").unwrap_or_default();

            let matched = first_similarity(&mentions, partial)
                .or_else(|| first_similarity(&chatters, partial))
                .or_else(|| first_similarity(&channels, partial))?;

            Some(format!("{before_at}@{matched} "))
        });

        let input = InputWidget::builder()
            .config(config.clone())
            .event_tx(event_tx.clone())
            .title("Chat (Ctrl+←/→: Word · Ctrl+Bksp: Del Word · ↑/↓: History · Tab: Complete)")
            .input_validator((storage.clone(), input_validator))
            .visual_indicator(visual_indicator)
            .input_suggester((storage.clone(), input_suggester))
            .build();

        let emote_picker = EmotePickerWidget::new(config.clone(), event_tx.clone(), emotes);

        Self {
            config,
            event_tx,
            storage,
            input,
            emote_picker,
            sent_history: Vec::new(),
            history_idx: None,
            draft: String::new(),
        }
    }

    pub const fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    pub const fn is_emote_picker_focused(&self) -> bool {
        self.emote_picker.is_focused()
    }

    #[must_use]
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.input.contains(column, row)
    }

    pub const fn toggle_focus(&mut self) {
        self.input.toggle_focus();
    }

    pub fn toggle_focus_with(&mut self, s: &str) {
        self.input.toggle_focus_with(s);
    }

    pub fn focus_with_mention(&mut self, username: &str) {
        let prefix = format!("@{username} ");
        if !self.input.is_focused() {
            self.input.focus_and_set(&prefix);
        } else {
            self.input.insert(&prefix);
        }
    }
}

impl Display for ChatInputWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.input)
    }
}

impl Component for ChatInputWidget {
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        self.input.draw(f, area)?;

        if self.emote_picker.is_focused() {
            self.emote_picker.draw(f, None)?;
        }

        Ok(())
    }

    async fn event(&mut self, event: &Event) -> Result<()> {
        if let Event::Internal(InternalEvent::SelectEmote(emote)) = event {
            self.input.insert(emote);
            self.input.insert(" ");
        }

        if self.emote_picker.is_focused() {
            return self.emote_picker.event(event).await;
        }

        if matches!(event, Event::Mouse(_)) {
            return self.input.event(event).await;
        }

        if let Event::Input(key) = event {
            let keybinds = &self.config.keybinds.insert;
            match key {
                key if keybinds.confirm_text_input.contains(key) => {
                    if self.input.is_valid() {
                        let current_input = self.input.to_string();

                        self.input.clear();
                        self.history_idx = None;
                        self.draft.clear();

                        if self.sent_history.last() != Some(&current_input) {
                            if self.sent_history.len() >= MAX_SENT_HISTORY {
                                self.sent_history.remove(0);
                            }
                            self.sent_history.push(current_input.clone());
                        }

                        if self.config.storage.mentions {
                            for word in current_input.split_whitespace() {
                                if let Some(mention) = word.strip_prefix('@') {
                                    let clean = mention.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                                    if !clean.is_empty() {
                                        self.storage
                                            .borrow_mut()
                                            .add("mentions", clean.to_string())
                                            .context("Failed to add mention")?;
                                    }
                                }
                            }
                        }

                        self.event_tx
                            .send(Event::Twitch(TwitchEvent::Action(TwitchAction::Message(
                                current_input,
                            ))))
                            .await?;
                    }
                }
                Key::Up | Key::Ctrl('p') => {
                    if !self.sent_history.is_empty() {
                        let next_idx = match self.history_idx {
                            None => {
                                self.draft = self.input.to_string();
                                self.sent_history.len() - 1
                            }
                            Some(idx) => idx.saturating_sub(1),
                        };
                        self.history_idx = Some(next_idx);
                        if let Some(msg) = self.sent_history.get(next_idx) {
                            self.input.set_text(msg);
                        }
                    }
                }
                Key::Down | Key::Ctrl('n') => {
                    if let Some(idx) = self.history_idx {
                        if idx + 1 < self.sent_history.len() {
                            let next_idx = idx + 1;
                            self.history_idx = Some(next_idx);
                            if let Some(msg) = self.sent_history.get(next_idx) {
                                self.input.set_text(msg);
                            }
                        } else {
                            self.history_idx = None;
                            let draft = self.draft.clone();
                            self.input.set_text(&draft);
                        }
                    }
                }
                key if keybinds.toggle_emote_picker.contains(key) => {
                    if self.config.frontend.is_emotes_enabled() {
                        self.emote_picker.toggle_focus();
                    }
                }
                key if keybinds.back_to_previous_window.contains(key) => {
                    self.input.toggle_focus();
                }
                _ => {
                    self.history_idx = None;
                    self.input.event(event).await?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};
    use tokio::sync::mpsc;

    use super::*;
    use crate::{config::CoreConfig, emotes::Emotes, handlers::storage::Storage};

    #[test]
    fn test_focus_with_mention_does_not_panic() {
        let config = Arc::new(CoreConfig::default());
        let (tx, _rx) = mpsc::channel(10);
        let storage = Rc::new(RefCell::new(Storage::new(&config).unwrap()));
        let emotes = Rc::new(Emotes::new(false));

        let mut widget = ChatInputWidget::new(config, tx, storage, emotes);
        assert!(!widget.is_focused());

        // First click when unfocused (previously panicked in LineBuffer::update("", 1))
        widget.focus_with_mention("BowmanDean");
        assert!(widget.is_focused());
        assert_eq!(widget.to_string(), "@BowmanDean ");

        // Second click while already focused appends another mention
        widget.focus_with_mention("boxxy_queen_of_b");
        assert_eq!(widget.to_string(), "@BowmanDean @boxxy_queen_of_b ");
    }

    #[tokio::test]
    async fn test_sent_history_and_word_navigation() {
        let config = Arc::new(CoreConfig::default());
        let (tx, mut rx) = mpsc::channel(10);
        let storage = Rc::new(RefCell::new(Storage::new(&config).unwrap()));
        let emotes = Rc::new(Emotes::new(false));

        let mut widget = ChatInputWidget::new(config, tx, storage, emotes);
        widget.focus_with_mention("alice");
        widget.input.insert("hello world test");
        assert_eq!(widget.to_string(), "@alice hello world test");

        // Ctrl+Backspace removes previous word ("test")
        widget
            .event(&Event::Input(Key::CtrlBackspace))
            .await
            .unwrap();
        assert_eq!(widget.to_string(), "@alice hello world ");

        // Send first message
        widget.event(&Event::Input(Key::Enter)).await.unwrap();
        assert_eq!(widget.to_string(), "");
        let _ = rx.recv().await;

        // Type a draft, press Up to recall sent message, Down to restore draft
        widget.input.insert("my draft");
        widget.event(&Event::Input(Key::Up)).await.unwrap();
        assert_eq!(widget.to_string(), "@alice hello world ");
        widget.event(&Event::Input(Key::Down)).await.unwrap();
        assert_eq!(widget.to_string(), "my draft");
    }
}
