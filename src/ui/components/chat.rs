use std::{collections::VecDeque, slice::Iter};

use chrono::Local;
use color_eyre::{Result, eyre::ContextCompat};
use tokio::sync::mpsc::Sender;
use tui::{
    Frame,
    crossterm::event::{MouseButton, MouseEvent, MouseEventKind},
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, TitlePosition},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::SharedMessages,
    config::{SharedCoreConfig, Theme},
    emotes::SharedEmotes,
    events::{Event, InternalEvent, TwitchAction, TwitchEvent},
    handlers::{
        audio::{AudioStatus, SharedAudio, ToastSeverity},
        data::MessageData,
        filters::SharedFilters,
        sidebar::{SharedSidebar, SidebarClickAction, TOKEN_GENERATOR_URL},
        state::State,
        storage::SharedStorage,
    },
    twitch::oauth::TwitchOauth,
    ui::components::{
        ChannelSwitcherWidget, ChatInputWidget, Component, FollowingWidget, MessageSearchWidget,
        utils::Scrolling,
    },
    utils::{
        styles::{DATETIME_DARK_STYLE, DATETIME_LIGHT_STYLE, NO_COLOR, TEXT_DARK_STYLE, TITLE_STYLE},
        text::{TitleStyle, format_time_divider_line, title_line, title_line_with_rects},
    },
};

pub struct ChatWidget {
    config: SharedCoreConfig,
    event_tx: Sender<Event>,
    messages: SharedMessages,
    storage: SharedStorage,
    chat_input: ChatInputWidget,
    channel_input: ChannelSwitcherWidget,
    search_input: MessageSearchWidget,
    following: FollowingWidget,
    filters: SharedFilters,
    audio: SharedAudio,
    sidebar: SharedSidebar,
    pub scroll_offset: Scrolling,
    pub current_channel_name: String,
    last_chat_rect: Option<Rect>,
    top_title_rects: Vec<Rect>,
    bottom_action_rects: Vec<Rect>,
    scroll_offset_rect: Option<Rect>,
    visible_row_authors: Vec<Option<String>>,
}

impl ChatWidget {
    #[expect(
        clippy::too_many_arguments,
        reason = "ChatWidget aggregates the shared state handles for the chat screen"
    )]
    pub fn new(
        config: SharedCoreConfig,
        twitch_oauth: TwitchOauth,
        event_tx: Sender<Event>,
        messages: SharedMessages,
        storage: &SharedStorage,
        emotes: &SharedEmotes,
        filters: SharedFilters,
        audio: SharedAudio,
        sidebar: SharedSidebar,
    ) -> Self {
        let current_channel_name = config.twitch.channel.clone();

        let chat_input: ChatInputWidget = ChatInputWidget::new(
            config.clone(),
            event_tx.clone(),
            storage.clone(),
            emotes.clone(),
        );
        let channel_input =
            ChannelSwitcherWidget::new(config.clone(), event_tx.clone(), storage.clone());
        let search_input = MessageSearchWidget::new(config.clone(), event_tx.clone());
        let following = FollowingWidget::new(config.clone(), twitch_oauth, event_tx.clone());

        let scroll_offset = Scrolling::new(config.frontend.inverted_scrolling);

        Self {
            config,
            event_tx,
            messages,
            storage: storage.clone(),
            chat_input,
            channel_input,
            search_input,
            following,
            filters,
            audio,
            sidebar,
            scroll_offset,
            current_channel_name,
            last_chat_rect: None,
            top_title_rects: Vec::new(),
            bottom_action_rects: Vec::new(),
            scroll_offset_rect: None,
            visible_row_authors: Vec::new(),
        }
    }

    pub async fn open_in_player(&self) -> Result<()> {
        let channel_name = self.current_channel_name.as_str();
        if self.config.frontend.view_command.is_empty() {
            webbrowser::open(format!(
            "https://player.twitch.tv/?channel={channel_name}&enableExtensions=true&parent=twitch.tv&quality=chunked",
            ).as_str())?;
        } else {
            self.event_tx
                .send(Event::Internal(InternalEvent::OpenStream(
                    channel_name.to_string(),
                )))
                .await?;
        }

        Ok(())
    }

    pub fn get_messages<'a>(
        &mut self,
        frame: &Frame,
        area: Rect,
        visible_items: &'a [MessageData],
    ) -> VecDeque<Line<'a>> {
        // Accounting for not all heights of rows to be the same due to text wrapping,
        // so extra space needs to be used in order to scroll correctly.
        let mut total_row_height: usize = 0;

        let mut messages = VecDeque::new();
        let mut row_authors = VecDeque::new();

        let mut general_chunk_height = area.height as usize;
        if !self.config.frontend.hide_chat_border {
            general_chunk_height -= 2;
        }

        let _ = frame;
        let message_chunk_width = area.width as usize;

        let group_secs = i64::try_from(self.config.frontend.time_group_minutes.saturating_mul(60))
            .unwrap_or(300);
        let divider_style = match self.config.frontend.theme {
            Theme::Dark => *DATETIME_DARK_STYLE,
            Theme::Light => *DATETIME_LIGHT_STYLE,
        };
        let now = Local::now();

        let username_highlight: Option<&str> = if self.config.frontend.username_highlight {
            Some(self.config.twitch.username.as_str())
        } else {
            None
        };
        let search_focused = self.search_input.is_focused();
        let search = if search_focused {
            self.search_input.to_string()
        } else {
            String::new()
        };

        'outer: for (idx, data) in visible_items.iter().enumerate() {
            let lines = data.to_vec(
                &self.config.frontend,
                message_chunk_width,
                if search_focused { Some(&search) } else { None },
                username_highlight,
            );

            let row_author = if !data.system && !data.author.is_empty() {
                Some(data.author.clone())
            } else {
                None
            };

            for span in lines.into_iter().rev() {
                if total_row_height < general_chunk_height {
                    messages.push_front(span);
                    row_authors.push_front(row_author.clone());
                    total_row_height += 1;
                } else {
                    break 'outer;
                }
            }

            if group_secs > 0 && total_row_height < general_chunk_height {
                let current_bucket = data.time_sent.timestamp().div_euclid(group_secs);
                let next_bucket = visible_items
                    .get(idx + 1)
                    .map(|older| older.time_sent.timestamp().div_euclid(group_secs));

                if next_bucket != Some(current_bucket) {
                    let divider = format_time_divider_line(
                        data.time_sent,
                        now,
                        message_chunk_width,
                        divider_style,
                    );
                    messages.push_front(divider);
                    row_authors.push_front(None);
                    total_row_height += 1;
                }
            }
        }

        // Padding with empty rows so chat can go from bottom to top.
        if general_chunk_height > total_row_height {
            for _ in 0..(general_chunk_height - total_row_height) {
                messages.push_front(Line::from(vec![Span::raw("")]));
                row_authors.push_front(None);
            }
        }

        self.visible_row_authors = row_authors.into_iter().collect();

        messages
    }

    fn status_badge(label: &'static str, active: bool) -> TitleStyle<'static> {
        let style = if *NO_COLOR {
            Style::default()
        } else {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(if active {
                    Color::Rgb(0x50, 0xC8, 0x78)
                } else {
                    Color::Rgb(0x94, 0x9C, 0xBB)
                })
        };
        TitleStyle::Custom(Span::styled(label, style))
    }

    fn audio_badge(&self) -> TitleStyle<'static> {
        let audio = self.audio.borrow();
        let label = audio.badge_label();
        let style = if *NO_COLOR {
            Style::default()
        } else {
            let fg = match audio.status() {
                AudioStatus::Muted => Color::Rgb(0x94, 0x9C, 0xBB),
                AudioStatus::Connecting { .. } => Color::Rgb(0x38, 0xBD, 0xF8),
                AudioStatus::Playing { .. } => Color::Rgb(0x50, 0xC8, 0x78),
                AudioStatus::Offline { .. } => Color::Rgb(0xF5, 0x9E, 0x0B),
                AudioStatus::Error { .. } => Color::Rgb(0xF8, 0x71, 0x71),
            };
            Style::default().add_modifier(Modifier::BOLD).fg(fg)
        };
        TitleStyle::Custom(Span::styled(label, style))
    }

    fn render_audio_toast(&self, f: &mut Frame, chat_area: Rect) {
        let Some(toast) = self.audio.borrow().active_toast() else {
            return;
        };
        if chat_area.width < 34 || chat_area.height < 6 {
            return;
        }

        let (border_fg, title_fg) = if *NO_COLOR {
            (Color::Reset, Color::Reset)
        } else {
            match toast.severity {
                ToastSeverity::Connecting => (
                    Color::Rgb(0x38, 0xBD, 0xF8),
                    Color::Rgb(0x38, 0xBD, 0xF8),
                ),
                ToastSeverity::Success => (
                    Color::Rgb(0x50, 0xC8, 0x78),
                    Color::Rgb(0x50, 0xC8, 0x78),
                ),
                ToastSeverity::Info => (
                    Color::Rgb(0xC4, 0xB5, 0xFD),
                    Color::Rgb(0xC4, 0xB5, 0xFD),
                ),
                ToastSeverity::Warning => (
                    Color::Rgb(0xF5, 0x9E, 0x0B),
                    Color::Rgb(0xF5, 0x9E, 0x0B),
                ),
                ToastSeverity::Error => (
                    Color::Rgb(0xF8, 0x71, 0x71),
                    Color::Rgb(0xF8, 0x71, 0x71),
                ),
            }
        };

        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let bg_color = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x16, 0x19, 0x26)
        };

        let content_w = UnicodeWidthStr::width(toast.message.as_str())
            .max(UnicodeWidthStr::width(toast.title.as_str()))
            .saturating_add(4);
        let max_w = usize::from(chat_area.width.saturating_sub(4)).max(24);
        let box_w = u16::try_from(content_w.clamp(28, max_w)).unwrap_or(36);
        let box_h = 3u16;

        let x = chat_area
            .right()
            .saturating_sub(box_w.saturating_add(2))
            .max(chat_area.x.saturating_add(1));
        let y = chat_area.y.saturating_add(1);
        let toast_rect = Rect::new(x, y, box_w, box_h);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(Style::default().fg(border_fg))
            .style(Style::default().bg(bg_color))
            .title(Line::from(Span::styled(
                toast.title,
                Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
            )));

        let para = Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(toast.message, Style::default().fg(text_primary)),
        ]))
        .block(block);

        f.render_widget(Clear, toast_rect);
        f.render_widget(para, toast_rect);
    }

    fn scroll_chat_up(&mut self) {
        let limit = self.scroll_offset.get_offset() < self.messages.borrow().len().saturating_sub(1);
        if limit {
            self.scroll_offset.up();
        } else if self.scroll_offset.is_inverted() {
            self.scroll_offset.down();
        }
    }

    fn scroll_chat_down(&mut self) {
        let limit = self.scroll_offset.get_offset() < self.messages.borrow().len().saturating_sub(1);
        if self.scroll_offset.is_inverted() {
            if limit {
                self.scroll_offset.up();
            }
        } else {
            self.scroll_offset.down();
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Dispatches mouse clicks and scroll wheel across chat pane, title badges, bottom bar, and sidebar"
    )]
    async fn handle_mouse_event(&mut self, mouse: &MouseEvent) -> Result<()> {
        let pos = Position::new(mouse.column, mouse.row);

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.sidebar.borrow().contains_following(mouse.column, mouse.row) {
                    self.sidebar.borrow_mut().scroll_up();
                } else {
                    self.scroll_chat_up();
                }
                return Ok(());
            }
            MouseEventKind::ScrollDown => {
                if self.sidebar.borrow().contains_following(mouse.column, mouse.row) {
                    self.sidebar.borrow_mut().scroll_down();
                } else {
                    self.scroll_chat_down();
                }
                return Ok(());
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Right-click on Filter badge reverses filters
                if self.top_title_rects.get(2).is_some_and(|r| r.contains(pos)) {
                    let mut filters = self.filters.borrow_mut();
                    filters.message.reverse();
                    filters.username.reverse();
                    return Ok(());
                }
                if self.chat_input.is_focused() {
                    self.chat_input.toggle_focus();
                } else if self.search_input.is_focused() {
                    self.search_input.toggle_focus();
                }
                return Ok(());
            }
            MouseEventKind::Down(MouseButton::Left) => {}
            _ => return Ok(()),
        }

        // Top title bar pills: [0]= twt (Time), [1]=Channel, [2]=Filter, [3]=Audio, [4]=Sidebar, [5]=?
        if let Some((idx, _)) = self
            .top_title_rects
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(pos))
        {
            match idx {
                0 => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(
                            State::Dashboard,
                        )))
                        .await?;
                }
                1 => self.channel_input.toggle_focus(),
                2 => {
                    let mut filters = self.filters.borrow_mut();
                    filters.message.toggle();
                    filters.username.toggle();
                }
                3 => self.audio.borrow_mut().toggle(),
                4 => self.sidebar.borrow_mut().toggle_visible(),
                5 => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(State::Help)))
                        .await?;
                }
                _ => {}
            }
            return Ok(());
        }

        // Bottom scroll-offset pill: jump to bottom
        if self.scroll_offset_rect.is_some_and(|r| r.contains(pos)) {
            self.scroll_offset.jump_to(0);
            return Ok(());
        }

        // Bottom action pills: [0]=Chat, [1]=@, [2]=/, [3]=Search, [4]=Dashboard
        if let Some((idx, _)) = self
            .bottom_action_rects
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(pos))
        {
            match idx {
                0 => self.chat_input.toggle_focus(),
                1 => {
                    if self.chat_input.is_focused() {
                        self.chat_input.toggle_focus();
                    }
                    self.chat_input.toggle_focus_with("@");
                }
                2 => {
                    if self.chat_input.is_focused() {
                        self.chat_input.toggle_focus();
                    }
                    self.chat_input.toggle_focus_with("/");
                }
                3 => self.search_input.toggle_focus(),
                4 => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(
                            State::Dashboard,
                        )))
                        .await?;
                }
                _ => {}
            }
            return Ok(());
        }

        // Sidebar clicks (Stream Info / Following channels / Search / Scope link)
        let sidebar_action = self.sidebar.borrow().handle_click(mouse.column, mouse.row);
        if let Some(action) = sidebar_action {
            match action {
                SidebarClickAction::OpenInPlayer => {
                    self.open_in_player().await?;
                }
                SidebarClickAction::JoinChannel(channel) => {
                    if self.config.storage.channels {
                        let _ = self
                            .storage
                            .borrow_mut()
                            .add("channels", channel.clone());
                    }
                    self.event_tx
                        .send(Event::Twitch(TwitchEvent::Action(
                            TwitchAction::JoinChannel(channel),
                        )))
                        .await?;
                }
                SidebarClickAction::OpenFollowingSearch => {
                    self.following.toggle_focus().await;
                }
                SidebarClickAction::OpenTokenGeneratorUrl => {
                    let _ = webbrowser::open(TOKEN_GENERATOR_URL);
                }
            }
            return Ok(());
        }

        // Chat pane click: bottom border opens chat input; clicking a message row mentions @author
        if let Some(chat_rect) = self.last_chat_rect {
            if chat_rect.contains(pos) {
                let border_offset = u16::from(!self.config.frontend.hide_chat_border);
                if !self.config.frontend.hide_chat_border
                    && mouse.row == chat_rect.bottom().saturating_sub(1)
                {
                    if !self.chat_input.is_focused() {
                        self.chat_input.toggle_focus();
                    }
                    return Ok(());
                }

                let inner_top = chat_rect.y.saturating_add(border_offset);
                let inner_bottom = chat_rect.bottom().saturating_sub(border_offset);
                if mouse.row >= inner_top && mouse.row < inner_bottom {
                    let row_idx = usize::from(mouse.row.saturating_sub(inner_top));
                    if let Some(Some(author)) = self.visible_row_authors.get(row_idx).cloned() {
                        self.chat_input.focus_with_mention(&author);
                        return Ok(());
                    }
                    if self.chat_input.is_focused() {
                        self.chat_input.toggle_focus();
                    } else if self.search_input.is_focused() {
                        self.search_input.toggle_focus();
                    }
                }
            }
        }

        Ok(())
    }
}

impl Component for ChatWidget {
    #[expect(
        clippy::too_many_lines,
        reason = "Chat draw renders both the chat pane and optional stream/following sidebar"
    )]
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        let full_area = area.unwrap_or_else(|| f.area());
        let sidebar_visible = self.sidebar.borrow().visible && full_area.width >= 85;
        let sidebar_width = self.sidebar.borrow().width;

        let (r, sidebar_rect) = if sidebar_visible {
            let h_split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(45), Constraint::Length(sidebar_width)])
                .split(full_area);
            (
                *h_split.first().context("Failed to get chat area")?,
                h_split.get(1).copied(),
            )
        } else {
            (full_area, None)
        };

        let mut v_constraints = vec![Constraint::Min(1)];

        if self.chat_input.is_focused() || self.search_input.is_focused() {
            v_constraints.push(Constraint::Length(3));
        }

        let v_chunks_binding = Layout::default()
            .direction(Direction::Vertical)
            .margin(self.config.frontend.margin)
            .constraints(v_constraints)
            .split(r);

        let mut v_chunks: Iter<Rect> = v_chunks_binding.iter();

        let first_v_chunk = *v_chunks
            .next()
            .context("Failed to get first vertical chunk")?;
        self.last_chat_rect = Some(first_v_chunk);

        if self.messages.borrow().len() > self.config.terminal.maximum_messages {
            self.messages
                .borrow_mut()
                .truncate(self.config.terminal.maximum_messages);
        }

        let (messages_len, visible_slice): (usize, Vec<MessageData>) = {
            let messages_ref = self.messages.borrow();
            let filters = self.filters.borrow();
            let scroll = self.scroll_offset.get_offset();
            let max_rows = usize::from(first_v_chunk.height).saturating_add(2);
            let slice = messages_ref
                .iter()
                .filter(|data| {
                    !filters.message.contaminated(&data.payload)
                        && !filters.username.contaminated(&data.author)
                })
                .skip(scroll)
                .take(max_rows)
                .cloned()
                .collect();
            (messages_ref.len(), slice)
        };

        let messages: Vec<ListItem> = self
            .get_messages(f, first_v_chunk, &visible_slice)
            .into_iter()
            .map(|item| ListItem::new(Text::from(item)))
            .collect();

        let current_time = if self.config.frontend.relative_time {
            Local::now().format("%H:%M").to_string()
        } else {
            Local::now()
                .format(&self.config.frontend.datetime_format)
                .to_string()
        };

        let (reversed_filters, enabled_filters) = {
            let filters = self.filters.borrow();
            (
                filters.message.is_reversed() || filters.username.is_reversed(),
                filters.message.is_enabled() || filters.username.is_enabled(),
            )
        };
        let sidebar_enabled = self.sidebar.borrow().visible;
        let audio_badge = self.audio_badge();

        let spans = [
            TitleStyle::Combined(" twt", &current_time),
            TitleStyle::Combined("Channel", self.current_channel_name.as_str()),
            Self::status_badge(
                if reversed_filters { "retliF" } else { "Filter" },
                enabled_filters,
            ),
            audio_badge,
            Self::status_badge("Sidebar", sidebar_enabled),
            TitleStyle::Single("?"),
        ];

        let chat_title = if self.config.frontend.title_shown {
            let (title_spans, rects) =
                title_line_with_rects(&spans, *TITLE_STYLE, first_v_chunk.x + 1, first_v_chunk.y);
            self.top_title_rects = rects;
            Line::from(title_spans)
        } else {
            self.top_title_rects.clear();
            Line::default()
        };

        let bottom_actions = [
            TitleStyle::Single("Chat"),
            TitleStyle::Single("@"),
            TitleStyle::Single("/"),
            TitleStyle::Single("Search"),
            TitleStyle::Single("Dashboard"),
        ];

        let list = if self.config.frontend.hide_chat_border {
            self.bottom_action_rects.clear();
            List::new(messages)
        } else {
            let (bottom_spans, bottom_rects) = title_line_with_rects(
                &bottom_actions,
                *TITLE_STYLE,
                first_v_chunk.x + 1,
                first_v_chunk.bottom().saturating_sub(1),
            );
            self.bottom_action_rects = bottom_rects;

            List::new(messages).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(self.config.frontend.border_type.clone().into())
                    .title(chat_title)
                    .title_bottom(Line::from(bottom_spans)),
            )
        }
        .style(*TEXT_DARK_STYLE);

        f.render_widget(list, first_v_chunk);
        self.render_audio_toast(f, first_v_chunk);

        if let Some(sb_area) = sidebar_rect {
            self.sidebar
                .borrow_mut()
                .draw(f, sb_area, &self.config, messages_len);
        }

        self.scroll_offset_rect = None;
        if self.config.frontend.show_scroll_offset || self.scroll_offset.get_offset() > 0 {
            // Cannot scroll past the first message
            let message_amount = messages_len.saturating_sub(1);

            let title_binding = format!(
                "{} / {}",
                self.scroll_offset.get_offset(),
                message_amount.to_string().as_str()
            );

            let title = [TitleStyle::Single(&title_binding)];
            let pill_w = u16::try_from(title_binding.len().saturating_add(4)).unwrap_or(10);
            let pill_x = first_v_chunk
                .right()
                .saturating_sub(pill_w.saturating_add(1));
            let pill_y = first_v_chunk.bottom().saturating_sub(1);
            self.scroll_offset_rect = Some(Rect::new(pill_x, pill_y, pill_w, 1));

            let bottom_block = Block::default()
                .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
                .border_type(self.config.frontend.border_type.clone().into())
                .title(title_line(&title, Style::default()))
                .title_position(TitlePosition::Bottom)
                .title_alignment(Alignment::Right);

            let rect = Rect::new(pill_x.saturating_sub(1), pill_y, pill_w.saturating_add(2), 1);

            f.render_widget(bottom_block, rect);
        }

        if self.chat_input.is_focused() {
            self.chat_input.draw(f, v_chunks.next().copied())?;
        } else if self.channel_input.is_focused() {
            self.channel_input.draw(f, None)?;
        } else if self.search_input.is_focused() {
            self.search_input.draw(f, v_chunks.next().copied())?;
        } else if self.following.is_focused() {
            self.following.draw(f, None)?;
        }

        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Chat event handler dispatches all normal-mode keybindings"
    )]
    async fn event(&mut self, event: &Event) -> Result<()> {
        if let Event::Twitch(TwitchEvent::Action(TwitchAction::JoinChannel(channel))) = event {
            self.current_channel_name.clone_from(channel);
        }

        if self.channel_input.is_focused() {
            return self.channel_input.event(event).await;
        } else if self.following.is_focused() {
            return self.following.event(event).await;
        } else if self.chat_input.is_emote_picker_focused() {
            return self.chat_input.event(event).await;
        }

        if let Event::Mouse(mouse) = event {
            if self.chat_input.is_focused() && self.chat_input.contains(mouse.column, mouse.row) {
                return self.chat_input.event(event).await;
            }
            if self.search_input.is_focused()
                && self.search_input.contains(mouse.column, mouse.row)
            {
                return self.search_input.event(event).await;
            }
            return self.handle_mouse_event(mouse).await;
        }

        if self.chat_input.is_focused() {
            return self.chat_input.event(event).await;
        } else if self.search_input.is_focused() {
            return self.search_input.event(event).await;
        }

        if let Event::Input(key) = event {
            let limit =
                self.scroll_offset.get_offset() < self.messages.borrow().len().saturating_sub(1);

            let keybinds = &self.config.keybinds.normal;
            match key {
                key if keybinds.enter_insert.contains(key) => self.chat_input.toggle_focus(),
                key if keybinds.enter_insert_with_mention.contains(key) => {
                    self.chat_input.toggle_focus_with("@");
                }
                key if keybinds.enter_insert_with_command.contains(key) => {
                    self.chat_input.toggle_focus_with("/");
                }
                key if keybinds.recent_channels_search.contains(key) => {
                    self.channel_input.toggle_focus();
                }
                key if keybinds.search_messages.contains(key) => {
                    self.search_input.toggle_focus();
                }
                key if keybinds.followed_channels_search.contains(key) => {
                    self.following.toggle_focus().await;
                }
                key if keybinds.toggle_filters.contains(key) => {
                    let mut filters = self.filters.borrow_mut();
                    filters.message.toggle();
                    filters.username.toggle();
                }
                key if keybinds.reverse_filters.contains(key) => {
                    let mut filters = self.filters.borrow_mut();
                    // TODO: Find a better way to interact with these filters
                    filters.message.reverse();
                    filters.username.reverse();
                }
                key if keybinds.enter_dashboard.contains(key) => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(
                            State::Dashboard,
                        )))
                        .await?;
                }
                key if keybinds.help.contains(key) => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(State::Help)))
                        .await?;
                }
                key if keybinds.quit.contains(key) => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::Quit))
                        .await?;
                }
                key if keybinds.open_in_player.contains(key) => {
                    self.open_in_player().await?;
                }
                key if keybinds.toggle_audio.contains(key) => {
                    self.audio.borrow_mut().toggle();
                }
                key if keybinds.toggle_sidebar.contains(key) => {
                    self.sidebar.borrow_mut().toggle_visible();
                }
                key if keybinds.scroll_to_end.contains(key) => {
                    self.scroll_offset.jump_to(0);
                }
                key if keybinds.scroll_to_start.contains(key) => {
                    // TODO: Make this not jump to nothingness
                    self.scroll_offset.jump_to(self.messages.borrow().len());
                }
                key if keybinds.back_to_previous_window.contains(key) => {
                    if self.scroll_offset.get_offset() == 0 {
                        self.event_tx
                            .send(Event::Internal(InternalEvent::BackOneLayer))
                            .await?;
                    }

                    self.scroll_offset.jump_to(0);
                }
                key if keybinds.scroll_up.contains(key) => {
                    if limit {
                        self.scroll_offset.up();
                    } else if self.scroll_offset.is_inverted() {
                        self.scroll_offset.down();
                    }
                }
                key if keybinds.scroll_down.contains(key) => {
                    if self.scroll_offset.is_inverted() {
                        if limit {
                            self.scroll_offset.up();
                        }
                    } else {
                        self.scroll_offset.down();
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}
