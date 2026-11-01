use std::{
    cell::RefCell,
    collections::VecDeque,
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use bon::bon;
use color_eyre::{
    Result,
    eyre::{Context, ContextCompat},
};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot::Receiver as OSReceiver,
};
use tracing::{error, warn};
use tui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::SharedCoreConfig,
    emotes::{
        ApplyCommand, DecodedEmote, DownloadedEmotes, Emotes, SharedEmotes, display_emote,
        query_emotes,
    },
    events::{Event, Events, InternalEvent, Key, TwitchAction, TwitchEvent, TwitchNotification},
    handlers::{
        audio::{SharedAudio, StreamAudio},
        data::{KNOWN_CHATTERS, MessageData},
        duckdb::{DuckDbStore, SharedDuckDb},
        filters::Filters,
        sidebar::{SharedSidebar, SidebarState, is_channel_live},
        state::State,
        storage::{SharedStorage, Storage},
    },
    twitch::oauth::TwitchOauth,
    ui::components::{Component, Components},
    utils::{sanitization::clean_channel_name, styles::NO_COLOR},
};

pub type SharedMessages = Rc<RefCell<VecDeque<MessageData>>>;

pub struct App {
    pub running: bool,

    /// UI components
    pub components: Components,

    /// Configuration loaded from file and CLI arguments
    pub config: SharedCoreConfig,

    /// Twitch OAuth client and session info
    pub twitch_oauth: TwitchOauth,
    pub events: Events,
    pub twitch_tx: Sender<TwitchAction>,

    pub messages: SharedMessages,

    /// Data loaded in from a JSON file.
    pub storage: SharedStorage,

    /// Persistent `DuckDB` message and event store.
    pub duckdb: SharedDuckDb,

    /// Background stream audio via streamlink.
    pub audio: SharedAudio,

    /// Vertical sidebar state (stream metadata & followed channels).
    pub sidebar: SharedSidebar,

    /// Current active channel name for `DuckDB` logging.
    current_channel: String,

    /// Channel currently connected and subscribed in `TwitchWebsocket` + `StreamAudio`.
    connected_channel: Option<String>,

    /// States
    state: State,
    previous_state: Option<State>,

    /// Emote encoding pipeline
    pub emotes: SharedEmotes,
    pub emotes_rx: OSReceiver<(DownloadedEmotes, DownloadedEmotes)>,
    pub decoded_emotes_rx: Option<Receiver<Result<DecodedEmote, String>>>,

    pub running_stream: Option<Child>,

    /// Whether the visual keybind overlay is currently shown
    pub keybind_overlay_active: bool,
    ///Whether the keybind overlay is pinned open (toggled with Space) vs hold-to-show
    pub keybind_overlay_pinned: bool,
    /// Last time a hold-to-show key event was received (for terminals without key-release events)
    pub keybind_overlay_last_hold: Option<Instant>,
}

macro_rules! shared {
    ($expression:expr) => {
        Rc::new(RefCell::new($expression))
    };
}

#[bon]
impl App {
    #[builder]
    pub fn new(
        config: SharedCoreConfig,
        twitch_oauth: TwitchOauth,
        events: Events,
        event_tx: Sender<Event>,
        twitch_tx: Sender<TwitchAction>,
        emotes: Rc<Emotes>,
        decoded_emotes_rx: Option<Receiver<Result<DecodedEmote, String>>>,
    ) -> Result<Self> {
        let maximum_messages = config.terminal.maximum_messages;
        let first_state = config.terminal.first_state.clone();

        let storage = shared!(Storage::new(&config)?);
        let duckdb = shared!(DuckDbStore::with_max_messages(
            crate::handlers::duckdb::default_duckdb_path(),
            maximum_messages,
        ));
        let filters = shared!(Filters::new(&config));
        let audio = shared!(StreamAudio::new(&config));
        let sidebar = shared!(SidebarState::new(&config));
        let initial_channel = storage
            .borrow()
            .last_channel()
            .unwrap_or_else(|| clean_channel_name(&config.twitch.channel));
        let initial_messages = duckdb
            .borrow()
            .load_channel_messages(&initial_channel, maximum_messages);
        let messages = shared!(initial_messages);

        {
            let mut tracked = config.frontend.favorite_channels.clone();
            tracked.extend(storage.borrow().followed_channels());
            let unfollowed = storage.borrow().unfollowed_channels();
            sidebar.borrow_mut().maybe_refresh(
                &initial_channel,
                tracked,
                unfollowed,
                &twitch_oauth,
                true,
            );
        }

        let mut components = Components::builder()
            .config(&config)
            .twitch_oauth(twitch_oauth.clone())
            .event_tx(event_tx.clone())
            .storage(storage.clone())
            .filters(filters)
            .audio(audio.clone())
            .sidebar(sidebar.clone())
            .messages(messages.clone())
            .emotes(&emotes)
            .build();

        components
            .chat
            .current_channel_name
            .clone_from(&initial_channel);
        components.state_tabs.select(&first_state);

        let connected_channel = if first_state == State::Normal {
            if config.frontend.audio {
                audio.borrow_mut().play(&initial_channel);
            }
            let _ = twitch_tx.try_send(TwitchAction::JoinChannel(initial_channel.clone()));
            Some(initial_channel.clone())
        } else {
            // Auto-resume the last active channel if it is currently live on Twitch
            let oauth_clone = twitch_oauth.clone();
            let candidate = initial_channel.clone();
            let tx_clone = event_tx;
            tokio::spawn(async move {
                if is_channel_live(&oauth_clone, &candidate).await {
                    let _ = tx_clone
                        .send(Event::Twitch(TwitchEvent::Action(
                            TwitchAction::JoinChannel(candidate),
                        )))
                        .await;
                }
            });
            None
        };

        let emotes_rx = query_emotes(&config, twitch_oauth.clone(), initial_channel.clone());

        let app = Self {
            running: true,
            components,
            config,
            twitch_oauth,
            events,
            twitch_tx,
            messages,
            storage,
            duckdb,
            audio,
            sidebar,
            current_channel: initial_channel,
            connected_channel,
            state: first_state,
            previous_state: None,
            emotes,
            emotes_rx,
            decoded_emotes_rx,
            running_stream: None,
            keybind_overlay_active: false,
            keybind_overlay_pinned: false,
            keybind_overlay_last_hold: None,
        };
        app.sync_window_title();
        Ok(app)
    }

    fn sync_window_title(&self) {
        let title = match self.state {
            State::Dashboard => {
                if let Some(ch) = &self.connected_channel {
                    format!(" twt • Browse (🔊 #{ch})")
                } else {
                    " twt • Browse".to_string()
                }
            }
            State::Normal => format!(" twt • #{}", self.current_channel),
            State::Help => format!(" twt • Help (#{})", self.current_channel),
        };
        crate::commands::set_terminal_window_title(&title);
    }

    fn open_stream(&mut self, channel: &str) -> Result<()> {
        self.close_current_stream()?;
        let view_command = &self.config.frontend.view_command;

        if let Some((command, args)) = view_command.split_first() {
            self.running_stream = Command::new(command.clone())
                .args(args)
                .arg(format!("https://twitch.tv/{channel}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_or_else(
                    |err| {
                        error!("error spawning view process: {err}");
                        None
                    },
                    Some,
                );
        }

        Ok(())
    }

    fn close_current_stream(&mut self) -> Result<()> {
        if let Some(process) = self.running_stream.as_mut() {
            process
                .kill()
                .inspect_err(|err| error!("failed to kill view process: {err}"))?;
        }
        self.running_stream = None;

        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        self.close_current_stream()?;
        self.audio.borrow_mut().stop();
        self.duckdb.borrow_mut().shutdown();
        self.storage
            .borrow_mut()
            .set_last_channel(&self.current_channel);
        self.storage.borrow().dump_data()?;
        self.emotes.unload();

        Ok(())
    }

    fn clear_messages(&mut self) {
        self.messages.borrow_mut().clear();

        self.components.chat.scroll_offset.jump_to(0);
    }

    fn purge_user_messages(&self, user_id: &str) {
        let messages = self
            .messages
            .borrow_mut()
            .iter()
            .filter(|&m| m.user_id.clone().is_none_or(|user| user != user_id))
            .cloned()
            .collect::<VecDeque<MessageData>>();

        self.messages.replace(messages);
    }

    fn remove_message_with(&self, message_id: &str) -> Result<()> {
        let index = self
            .messages
            .borrow_mut()
            .iter()
            .position(|f| f.message_id.clone().is_some_and(|id| id == message_id));

        if let Some(i) = index {
            self.messages.borrow_mut().remove(i).context(format!(
                "Failed to remove message with ID {message_id} at index {i}"
            ))?;
        }

        Ok(())
    }

    fn get_previous_state(&self) -> Option<State> {
        self.previous_state.clone()
    }

    fn set_state(&mut self, other: State) {
        self.previous_state = Some(self.state.clone());
        self.state = other;

        self.components.state_tabs.select(&self.state);
        self.sync_window_title();
    }

    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let is_emotes_enabled = self.emotes.enabled;
        let mut needs_redraw = true;
        let mut last_periodic_redraw = std::time::Instant::now();

        while self.running {
            if needs_redraw {
                let mut draw_result = Ok(());
                terminal.draw(|f| {
                    draw_result = self.draw(f, Some(f.area()));
                })?;
                draw_result?;
                needs_redraw = false;
                last_periodic_redraw = std::time::Instant::now();
            }

            if let Some(event) = self.events.next().await {
                if self.handle_event(&event).await? {
                    needs_redraw = true;
                }
            }

            if is_emotes_enabled && self.handle_emote_event() {
                needs_redraw = true;
            }

            // Refresh relative time labels ("Xm ago") and header clock at least every 5s
            if last_periodic_redraw.elapsed() >= std::time::Duration::from_secs(5) {
                needs_redraw = true;
            }
        }

        self.cleanup()?;

        Ok(())
    }

    fn handle_emote_event(&mut self) -> bool {
        let mut changed = false;
        // Check if we have received any emotes
        if let Ok((user_emotes, global_emotes)) = self.emotes_rx.try_recv() {
            *self.emotes.user_emotes.borrow_mut() = user_emotes;
            *self.emotes.global_emotes.borrow_mut() = global_emotes;

            for message in &mut *self.messages.borrow_mut() {
                message.reparse_emotes(&self.emotes);
            }
            changed = true;
        }

        // Check if we need to load a decoded emote
        if let Some(rx) = &mut self.decoded_emotes_rx {
            while let Ok(r) = rx.try_recv() {
                changed = true;
                match r {
                    Ok(d) => {
                        if let Err(e) = d.apply() {
                            warn!("Unable to send command to load emote. {e}");
                        } else if let Err(e) = display_emote(d.id(), 1, d.cols()) {
                            warn!("Unable to send command to display emote. {e}");
                        }
                    }
                    Err(name) => {
                        warn!("Unable to load emote: {name}.");
                        self.emotes.user_emotes.borrow_mut().remove(&name);
                        self.emotes.global_emotes.borrow_mut().remove(&name);
                        self.emotes.info.borrow_mut().remove(&name);
                    }
                }
            }
        }

        changed
    }

    fn handle_internal_event(&mut self, internal_event: &InternalEvent) -> Result<()> {
        match internal_event {
            InternalEvent::Quit => self.running = false,
            InternalEvent::BackOneLayer => {
                if let Some(previous_state) = self.get_previous_state() {
                    self.set_state(previous_state);
                } else {
                    self.set_state(self.config.terminal.first_state.clone());
                }
            }
            InternalEvent::SwitchState(state) => {
                self.set_state(state.clone());
            }
            InternalEvent::OpenStream(channel) => {
                self.open_stream(channel)?;
            }
            InternalEvent::SelectEmote(_) => {}
        }

        Ok(())
    }

    async fn handle_twitch_action(&mut self, twitch_action: &TwitchAction) -> Result<()> {
        match twitch_action {
            TwitchAction::JoinChannel(channel) => {
                let channel = clean_channel_name(channel);

                // If we are already connected to this exact channel, keep existing in-memory
                // messages, EventSub subscriptions, and audio stream alive—just switch to Chat view.
                if self
                    .connected_channel
                    .as_ref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&channel))
                {
                    self.set_state(State::Normal);
                    return Ok(());
                }

                self.current_channel.clone_from(&channel);
                self.connected_channel = Some(channel.clone());
                self.storage.borrow_mut().set_last_channel(&channel);
                let restored = self
                    .duckdb
                    .borrow()
                    .load_channel_messages(&channel, self.config.terminal.maximum_messages);
                self.messages.replace(restored);
                self.components.chat.scroll_offset.jump_to(0);
                self.emotes.unload();

                self.duckdb.borrow().record_event(
                    &channel,
                    "join_channel",
                    Some(&self.config.twitch.username),
                    None,
                    None,
                    &channel,
                );

                self.twitch_tx
                    .send(TwitchAction::JoinChannel(channel.clone()))
                    .await?;

                if self.config.frontend.autostart_view_command {
                    self.open_stream(&channel)?;
                }
                self.audio.borrow_mut().set_channel(&channel);
                {
                    let mut tracked = self.config.frontend.favorite_channels.clone();
                    tracked.extend(self.storage.borrow().followed_channels());
                    let unfollowed = self.storage.borrow().unfollowed_channels();
                    self.sidebar.borrow_mut().maybe_refresh(
                        &channel,
                        tracked,
                        unfollowed,
                        &self.twitch_oauth,
                        true,
                    );
                }
                self.emotes_rx = query_emotes(&self.config, self.twitch_oauth.clone(), channel);
                self.set_state(State::Normal);
            }
            TwitchAction::Message(message) => {
                self.duckdb.borrow().record_event(
                    &self.current_channel,
                    "send_message",
                    Some(&self.config.twitch.username),
                    None,
                    None,
                    message,
                );
                self.twitch_tx
                    .send(TwitchAction::Message(message.clone()))
                    .await?;
            }
        }

        Ok(())
    }

    fn handle_twitch_notification(
        &mut self,
        twitch_notification: &TwitchNotification,
    ) -> Result<()> {
        match twitch_notification {
            TwitchNotification::Message(m) => {
                let message_data = MessageData::from_twitch_message(m.clone(), &self.emotes);
                if !KNOWN_CHATTERS.contains(&message_data.author.as_str())
                    && self.config.twitch.username != message_data.author
                {
                    self.storage
                        .borrow_mut()
                        .add("chatters", message_data.author.clone())
                        .context(format!(
                            "Failed to add chatter {} to storage",
                            message_data.author
                        ))?;
                }
                self.duckdb.borrow_mut().record_message(
                    &self.current_channel,
                    &message_data,
                    self.config.terminal.maximum_messages,
                );
                self.messages.borrow_mut().push_front(message_data);

                // If scrolling is enabled, pad for more messages.
                if self.components.chat.scroll_offset.get_offset() > 0 {
                    self.components.chat.scroll_offset.up();
                }
            }
            TwitchNotification::ClearChat(user_id) => {
                self.duckdb.borrow().record_event(
                    &self.current_channel,
                    "clear_chat",
                    None,
                    user_id.as_deref(),
                    None,
                    "clear_chat",
                );
                if let Some(user) = user_id {
                    self.purge_user_messages(user.as_str());
                } else {
                    self.clear_messages();
                }
            }
            TwitchNotification::DeleteMessage(message_id) => {
                self.duckdb.borrow().record_event(
                    &self.current_channel,
                    "delete_message",
                    None,
                    None,
                    Some(message_id.as_str()),
                    "delete_message",
                );
                self.remove_message_with(message_id.as_str())?;
            }
        }

        Ok(())
    }
}

impl Component for App {
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        let mut area = area.unwrap_or_else(|| f.area());

        if self.config.frontend.state_tabs {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(area.height - 1), Constraint::Length(1)])
                .split(f.area());

            area = *layout.first().context("Failed to get state tabs area")?;

            let rect = layout.last().context("Failed to get state tabs area")?;
            self.components.state_tabs.draw(f, Some(*rect))?;
        }

        if (area.height < 10 || area.width < 60)
            && self.config.frontend.show_unsupported_screen_size
        {
            self.components.window_size_error.draw(f, Some(f.area()))?;
        } else {
            match self.state {
                State::Dashboard => self.components.dashboard.draw(f, Some(area))?,
                State::Normal => self.components.chat.draw(f, Some(area))?,
                State::Help => self.components.help.draw(f, Some(area))?,
            }
        }

        if self.components.debug.is_focused() {
            let new_rect = Rect::new(area.x, area.y + 1, area.width - 1, area.height - 2);

            let rects = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(new_rect);
            let rect = rects.get(1).context("Failed to get debug area")?;

            self.components.debug.draw(f, Some(*rect))?;
        }

        if self.keybind_overlay_active {
            self.draw_keybind_overlay(f);
        }

        Ok(())
    }

    async fn event(&mut self, event: &Event) -> Result<()> {
        let _ = self.handle_event(event).await?;
        Ok(())
    }
}

impl App {
    fn is_any_input_focused(&self) -> bool {
        if self.components.debug.is_focused() {
            return true;
        }
        match self.state {
            State::Dashboard => self.components.dashboard.is_any_input_focused(),
            State::Normal => self.components.chat.is_any_input_focused(),
            State::Help => false,
        }
    }

    fn draw_keybind_overlay(&self, f: &mut Frame) {
        let full = f.area();
        let mut badges: Vec<(Rect, String)> = Vec::new();

        match self.state {
            State::Dashboard => badges.extend(self.components.dashboard.keybind_badges()),
            State::Normal => badges.extend(self.components.chat.keybind_badges()),
            State::Help => {}
        }

        if self.config.frontend.state_tabs {
            badges.extend(self.components.state_tabs.keybind_badges());
        }

        let badge_style = if *NO_COLOR {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .bg(Color::Rgb(0xF5, 0x9E, 0x0B))
                .fg(Color::Rgb(0x0F, 0x11, 0x1A))
                .add_modifier(Modifier::BOLD)
        };

        for (target_rect, label) in badges {
            let text = format!(" ⌨ {label} ");
            let w = u16::try_from(UnicodeWidthStr::width(text.as_str()))
                .unwrap_or(10)
                .min(full.width);
            if w == 0 || target_rect.y >= full.bottom() {
                continue;
            }
            let x = target_rect.x.min(full.right().saturating_sub(w));
            let badge_rect = Rect::new(x, target_rect.y, w, 1);
            f.render_widget(Clear, badge_rect);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(text, badge_style))),
                badge_rect,
            );
        }

        // Top-center floating banner explaining hold/toggle state
        let banner_text = if self.keybind_overlay_pinned {
            " ⌨ KEYBIND OVERLAY PINNED — Press [Space], [`], or [Esc] to dismiss "
        } else {
            " ⌨ KEYBIND OVERLAY (Hold [`] or [Space] to view · Release to hide) "
        };
        let bw = u16::try_from(UnicodeWidthStr::width(banner_text))
            .unwrap_or(50)
            .min(full.width);
        if bw > 0 && full.height >= 2 {
            let bx = full.x.saturating_add(full.width.saturating_sub(bw) / 2);
            let banner_rect = Rect::new(bx, full.y, bw, 1);
            let banner_style = if *NO_COLOR {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
                    .bg(Color::Rgb(0x91, 0x46, 0xFF))
                    .fg(Color::Rgb(0xFF, 0xFF, 0xFF))
                    .add_modifier(Modifier::BOLD)
            };
            f.render_widget(Clear, banner_rect);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(banner_text, banner_style)))
                    .alignment(Alignment::Center),
                banner_rect,
            );
        }
    }

    async fn handle_event(&mut self, event: &Event) -> Result<bool> {
        let mut should_redraw = !matches!(event, Event::Tick);

        match event {
            Event::Internal(internal_event) => {
                self.handle_internal_event(internal_event)?;
            }
            Event::Twitch(twitch_event) => match twitch_event {
                TwitchEvent::Action(twitch_action) => {
                    self.handle_twitch_action(twitch_action).await?;
                }
                TwitchEvent::Notification(twitch_notification) => {
                    self.handle_twitch_notification(twitch_notification)?;
                }
            },
            Event::KeyRelease(key) => {
                if matches!(key, Key::Char('`' | '~' | ' ')) && !self.keybind_overlay_pinned {
                    self.keybind_overlay_active = false;
                    self.keybind_overlay_last_hold = None;
                    return Ok(true);
                }
                return Ok(false);
            }
            Event::Input(key) => {
                if self.components.debug.is_focused() {
                    self.components.debug.event(event).await?;
                    return Ok(true);
                }

                if self.config.keybinds.toggle_debug_focus.contains(key) {
                    self.components.debug.toggle_focus();
                }

                if !self.is_any_input_focused() {
                    match key {
                        Key::Char('`' | '~') => {
                            self.keybind_overlay_active = true;
                            self.keybind_overlay_pinned = false;
                            self.keybind_overlay_last_hold = Some(Instant::now());
                            return Ok(true);
                        }
                        Key::Char(' ') => {
                            if self.keybind_overlay_active && self.keybind_overlay_last_hold.is_some_and(|t| t.elapsed() < Duration::from_millis(600)) {
                                // Holding Space down (key repeat) -> switch to hold-to-show mode
                                self.keybind_overlay_pinned = false;
                                self.keybind_overlay_last_hold = Some(Instant::now());
                            } else if self.keybind_overlay_active && self.keybind_overlay_pinned {
                                self.keybind_overlay_active = false;
                                self.keybind_overlay_pinned = false;
                                self.keybind_overlay_last_hold = None;
                            } else {
                                self.keybind_overlay_active = true;
                                self.keybind_overlay_pinned = true;
                                self.keybind_overlay_last_hold = Some(Instant::now());
                            }
                            return Ok(true);
                        }
                        Key::Esc if self.keybind_overlay_active => {
                            self.keybind_overlay_active = false;
                            self.keybind_overlay_pinned = false;
                            self.keybind_overlay_last_hold = None;
                            return Ok(true);
                        }
                        _ => {
                            if self.keybind_overlay_active {
                                self.keybind_overlay_active = false;
                                self.keybind_overlay_pinned = false;
                                self.keybind_overlay_last_hold = None;
                            }
                        }
                    }
                }
            }
            Event::Mouse(mouse) => {
                if self.config.frontend.state_tabs
                    && matches!(
                        mouse.kind,
                        tui::crossterm::event::MouseEventKind::Down(
                            tui::crossterm::event::MouseButton::Left
                        )
                    )
                {
                    if let Some(state) = self
                        .components
                        .state_tabs
                        .clicked_state(mouse.column, mouse.row)
                    {
                        self.set_state(state);
                        return Ok(true);
                    }
                }
            }
            Event::Resize => {
                return Ok(true);
            }
            Event::Tick => {
                if self.keybind_overlay_active
                    && !self.keybind_overlay_pinned
                    && self
                        .keybind_overlay_last_hold
                        .is_some_and(|t| t.elapsed() >= Duration::from_millis(850))
                {
                    self.keybind_overlay_active = false;
                    self.keybind_overlay_last_hold = None;
                    should_redraw = true;
                }
                if self.audio.borrow_mut().poll_status() {
                    should_redraw = true;
                }
                if self.components.dashboard.poll_updates() {
                    should_redraw = true;
                }
                let mut sb = self.sidebar.borrow_mut();
                if sb.poll_updates() {
                    should_redraw = true;
                }
                if sb.is_refresh_needed(&self.current_channel, false) {
                    let mut tracked = self.config.frontend.favorite_channels.clone();
                    tracked.extend(self.storage.borrow().followed_channels());
                    let unfollowed = self.storage.borrow().unfollowed_channels();
                    if sb.maybe_refresh(
                        &self.current_channel,
                        tracked,
                        unfollowed,
                        &self.twitch_oauth,
                        false,
                    ) {
                        should_redraw = true;
                    }
                }
            }
        }

        match self.state {
            State::Dashboard => self.components.dashboard.event(event).await?,
            State::Normal => self.components.chat.event(event).await?,
            State::Help => self.components.help.event(event).await?,
        }

        Ok(should_redraw)
    }
}
