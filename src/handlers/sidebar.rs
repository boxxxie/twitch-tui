use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    fmt::Write as _,
    rc::Rc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tui::{
    Frame,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::{
    config::SharedCoreConfig,
    twitch::{api::TWITCH_API_BASE_URL, oauth::TwitchOauth},
    utils::{
        styles::{BORDER_DARK_STYLE, NO_COLOR, TEXT_DARK_STYLE, TITLE_STYLE},
        text::{TitleStyle, title_line_with_rects},
    },
};

pub type SharedSidebar = Rc<RefCell<SidebarState>>;

const REFRESH_INTERVAL: Duration = Duration::from_secs(45);

pub const TOKEN_GENERATOR_URL: &str = "https://twitchtokengenerator.com/?scope=chat:read+chat:edit+channel:moderate+user:read:follows+user:read:emotes+user:read:chat+user:write:chat+moderator:manage:chat_messages+moderator:manage:banned_users+moderator:manage:chat_settings+moderator:manage:shoutouts+channel:manage:broadcast+channel:manage:moderators+channel:manage:vips+channel:manage:raids+channel:edit:commercial&auth=auth_stay";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarClickAction {
    OpenInPlayer,
    JoinChannel(String),
    ToggleFollow(String),
    OpenFollowingSearch,
    OpenTokenGeneratorUrl,
}

#[derive(Debug, Clone, Default)]
pub struct StreamMetadata {
    pub user_login: String,
    pub user_name: String,
    pub game_name: String,
    pub title: String,
    pub viewer_count: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    pub is_live: bool,
}

impl StreamMetadata {
    #[must_use]
    pub fn uptime_text(&self, now: DateTime<Utc>) -> String {
        let Some(started) = self.started_at else {
            return String::new();
        };
        let secs = now.signed_duration_since(started).num_seconds().max(0);
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if hours > 0 {
            format!("{hours}h{mins:02}m")
        } else {
            format!("{mins}m")
        }
    }

    #[must_use]
    pub fn viewers_compact(&self) -> String {
        if self.viewer_count >= 1_000_000 {
            format!("{:.1}M", self.viewer_count as f64 / 1_000_000.0)
        } else if self.viewer_count >= 1_000 {
            format!("{:.1}k", self.viewer_count as f64 / 1_000.0)
        } else {
            self.viewer_count.to_string()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CategoryMetadata {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct SidebarSnapshot {
    pub current_stream: Option<StreamMetadata>,
    pub live_channels: Vec<StreamMetadata>,
    pub offline_channels: Vec<String>,
    pub top_streams: Vec<StreamMetadata>,
    pub top_streams_cursor: Option<String>,
    pub top_categories: Vec<CategoryMetadata>,
    pub missing_follows_scope: bool,
}

#[derive(Debug)]
pub struct SidebarState {
    pub visible: bool,
    pub width: u16,
    pub current_channel: String,
    pub snapshot: SidebarSnapshot,
    pub scroll_offset: usize,
    stream_info_rect: Option<Rect>,
    follow_btn_rect: Option<(Rect, String)>,
    following_rect: Option<Rect>,
    following_header_rect: Option<Rect>,
    channel_hitboxes: Vec<(Rect, String)>,
    unfollow_hitboxes: Vec<(Rect, String)>,
    scope_hint_rect: Option<Rect>,
    last_refresh: Option<Instant>,
    tx: UnboundedSender<SidebarSnapshot>,
    rx: UnboundedReceiver<SidebarSnapshot>,
}

#[derive(Deserialize, Debug)]
struct HelixStreamItem {
    user_login: String,
    user_name: String,
    game_name: String,
    title: String,
    viewer_count: u64,
    started_at: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Debug, Default)]
struct HelixPagination {
    cursor: Option<String>,
}

#[derive(Deserialize, Debug)]
struct HelixStreamList {
    data: Vec<HelixStreamItem>,
    #[serde(default)]
    pagination: HelixPagination,
}

#[derive(Deserialize, Debug)]
struct HelixFollowedChannel {
    broadcaster_login: String,
}

#[derive(Deserialize, Debug)]
struct HelixFollowedChannelList {
    data: Vec<HelixFollowedChannel>,
    #[serde(default)]
    pagination: HelixPagination,
}

#[derive(Deserialize, Debug)]
struct HelixUserItem {
    id: String,
    login: String,
    display_name: String,
}

#[derive(Deserialize, Debug)]
struct HelixUserList {
    data: Vec<HelixUserItem>,
}

#[derive(Deserialize, Debug)]
struct HelixChannelInfoItem {
    broadcaster_login: String,
    broadcaster_name: String,
    game_name: String,
    title: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct HelixChannelInfoList {
    data: Vec<HelixChannelInfoItem>,
}

#[derive(Deserialize, Debug)]
struct HelixGameItem {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug)]
struct HelixGameList {
    data: Vec<HelixGameItem>,
}

#[derive(Deserialize, Debug)]
struct HelixSearchChannelItem {
    broadcaster_login: String,
    display_name: String,
    game_name: String,
    title: String,
    is_live: bool,
    started_at: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct HelixSearchChannelList {
    data: Vec<HelixSearchChannelItem>,
    #[serde(default)]
    pagination: HelixPagination,
}

impl SidebarState {
    #[must_use]
    pub fn new(config: &SharedCoreConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            visible: config.frontend.sidebar,
            width: config.frontend.sidebar_width.max(28),
            current_channel: config.twitch.channel.to_lowercase(),
            snapshot: SidebarSnapshot::default(),
            scroll_offset: 0,
            stream_info_rect: None,
            follow_btn_rect: None,
            following_rect: None,
            following_header_rect: None,
            channel_hitboxes: Vec::new(),
            unfollow_hitboxes: Vec::new(),
            scope_hint_rect: None,
            last_refresh: None,
            tx,
            rx,
        }
    }

    pub const fn toggle_visible(&mut self) {
        self.visible = !self.visible;
    }

    #[must_use]
    pub fn contains_following(&self, column: u16, row: u16) -> bool {
        let pos = Position::new(column, row);
        self.visible && self.following_rect.is_some_and(|r| r.contains(pos))
    }

    pub fn scroll_down(&mut self) {
        let total = self.snapshot.live_channels.len() + self.snapshot.offline_channels.len();
        if self.scroll_offset + 1 < total {
            self.scroll_offset += 1;
        }
    }

    pub const fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    #[must_use]
    pub fn handle_click(&self, column: u16, row: u16) -> Option<SidebarClickAction> {
        if !self.visible {
            return None;
        }
        let pos = Position::new(column, row);

        if let Some((rect, channel)) = &self.follow_btn_rect {
            if rect.contains(pos) {
                return Some(SidebarClickAction::ToggleFollow(channel.clone()));
            }
        }

        if self.following_header_rect.is_some_and(|r| r.contains(pos)) {
            return Some(SidebarClickAction::OpenFollowingSearch);
        }

        if self.scope_hint_rect.is_some_and(|r| r.contains(pos)) {
            return Some(SidebarClickAction::OpenTokenGeneratorUrl);
        }

        for (rect, channel) in &self.unfollow_hitboxes {
            if rect.contains(pos) {
                return Some(SidebarClickAction::ToggleFollow(channel.clone()));
            }
        }

        for (rect, channel) in &self.channel_hitboxes {
            if rect.contains(pos) {
                return Some(SidebarClickAction::JoinChannel(channel.clone()));
            }
        }

        if self.stream_info_rect.is_some_and(|r| r.contains(pos)) {
            return Some(SidebarClickAction::OpenInPlayer);
        }

        None
    }

    #[must_use]
    pub fn keybind_badges(&self) -> Vec<(Rect, String)> {
        if !self.visible {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(r) = self.stream_info_rect {
            out.push((
                Rect::new(r.x.saturating_add(1), r.y, 14.min(r.width), 1),
                "o: Watch".to_string(),
            ));
        }
        if let Some((r, _)) = &self.follow_btn_rect {
            out.push((*r, "u: Follow".to_string()));
        }
        if let Some(r) = self.following_header_rect {
            out.push((
                Rect::new(r.x.saturating_add(1), r.y, 16.min(r.width), 1),
                "f: Following".to_string(),
            ));
        }
        if let Some((r, _)) = self.channel_hitboxes.first() {
            out.push((
                Rect::new(r.x, r.y, 14.min(r.width), 1),
                "Click: Join".to_string(),
            ));
        }
        if let Some((r, _)) = self.unfollow_hitboxes.first() {
            out.push((
                Rect::new(r.x.saturating_sub(6), r.y, 10, 1),
                "×: Unf".to_string(),
            ));
        }
        out
    }

    pub fn poll_updates(&mut self) -> bool {
        let mut updated = false;
        while let Ok(snap) = self.rx.try_recv() {
            self.snapshot = snap;
            updated = true;
        }
        updated
    }

    pub fn is_refresh_needed(&self, current_channel: &str, force: bool) -> bool {
        if force || !current_channel.eq_ignore_ascii_case(&self.current_channel) {
            return true;
        }
        self.last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL)
    }

    pub fn is_followed(&self, channel: &str) -> bool {
        let clean = channel.trim().trim_start_matches('#');
        self.snapshot
            .live_channels
            .iter()
            .any(|s| s.user_login.eq_ignore_ascii_case(clean))
            || self
                .snapshot
                .offline_channels
                .iter()
                .any(|c| c.eq_ignore_ascii_case(clean))
    }

    pub fn remove_followed_immediately(&mut self, channel: &str) {
        let clean = channel.trim().trim_start_matches('#');
        self.snapshot
            .live_channels
            .retain(|s| !s.user_login.eq_ignore_ascii_case(clean));
        self.snapshot
            .offline_channels
            .retain(|c| !c.eq_ignore_ascii_case(clean));
    }

    pub fn add_followed_immediately(&mut self, channel: &str) {
        let clean = channel.trim().trim_start_matches('#').to_lowercase();
        if clean.is_empty() || self.is_followed(&clean) {
            return;
        }
        if let Some(meta) = self
            .snapshot
            .top_streams
            .iter()
            .find(|s| s.user_login.eq_ignore_ascii_case(&clean))
            .cloned()
        {
            self.snapshot.live_channels.push(meta);
            self.snapshot
                .live_channels
                .sort_by_key(|a| std::cmp::Reverse(a.viewer_count));
        } else if let Some(cur) = &self.snapshot.current_stream {
            if cur.user_login.eq_ignore_ascii_case(&clean) && cur.is_live {
                self.snapshot.live_channels.push(cur.clone());
                self.snapshot
                    .live_channels
                    .sort_by_key(|a| std::cmp::Reverse(a.viewer_count));
            } else {
                self.snapshot.offline_channels.push(clean);
                self.snapshot.offline_channels.sort();
            }
        } else {
            self.snapshot.offline_channels.push(clean);
            self.snapshot.offline_channels.sort();
        }
    }

    pub fn maybe_refresh(
        &mut self,
        current_channel: &str,
        tracked_channels: Vec<String>,
        unfollowed_channels: Vec<String>,
        twitch_oauth: &TwitchOauth,
        force: bool,
    ) -> bool {
        let updated = self.poll_updates();

        let channel_changed = !current_channel.eq_ignore_ascii_case(&self.current_channel);
        if channel_changed {
            self.current_channel = current_channel.to_lowercase();
        }

        let due = self
            .last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL);

        if !force && !channel_changed && !due {
            return updated;
        }

        let Some(client) = twitch_oauth.client() else {
            return updated;
        };

        self.last_refresh = Some(Instant::now());
        let tx = self.tx.clone();
        let active_channel = self.current_channel.clone();
        let user_id = twitch_oauth.user_id();
        let has_follows_scope = twitch_oauth.has_scope("user:read:follows");

        tokio::spawn(async move {
            let snap = fetch_sidebar_snapshot(
                &client,
                &active_channel,
                tracked_channels,
                unfollowed_channels,
                user_id.as_deref(),
                has_follows_scope,
            )
            .await;
            let _ = tx.send(snap);
        });

        updated
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, config: &SharedCoreConfig, msg_count: usize) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(13), Constraint::Min(1)])
            .split(area);

        if let Some(&top_area) = chunks.first() {
            self.draw_stream_info(f, top_area, config, msg_count);
        }
        if let Some(&bottom_area) = chunks.get(1) {
            self.draw_following_list(f, bottom_area, config);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders the Twitch Glitch + TWT block-art logo alongside live stream metadata"
    )]
    fn draw_stream_info(
        &mut self,
        f: &mut Frame,
        area: Rect,
        config: &SharedCoreConfig,
        msg_count: usize,
    ) {
        self.stream_info_rect = Some(area);
        let now_utc = Utc::now();
        let is_live = self
            .snapshot
            .current_stream
            .as_ref()
            .is_some_and(|s| s.is_live);

        let emerald = Color::Rgb(0x50, 0xC8, 0x78);
        let sky_cyan = Color::Rgb(0x38, 0xBD, 0xF8);
        let lavender = Color::Rgb(0xA7, 0x8B, 0xFA);
        let purple = Color::Rgb(0x91, 0x46, 0xFF);
        let purple_bright = Color::Rgb(0xA9, 0x70, 0xFF);
        let purple_deep = Color::Rgb(0x5C, 0x16, 0xC5);
        let text_bright = Color::Rgb(0xEC, 0xF0, 0xFF);
        let text_secondary = Color::Rgb(0xC0, 0xCA, 0xF5);
        let text_muted = Color::Rgb(0x94, 0x9C, 0xBB);

        let status_span = Span::styled(
            if is_live { "● LIVE" } else { "○ OFFLINE" },
            if *NO_COLOR {
                Style::default()
            } else {
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(if is_live { emerald } else { text_muted })
            },
        );

        let title_spans = [
            TitleStyle::Single(" twt"),
            TitleStyle::Custom(status_span),
            TitleStyle::Single("Watch"),
            TitleStyle::Single(if self.is_followed(&self.current_channel) {
                "♥ Following (u)"
            } else {
                "♡ Follow (u)"
            }),
        ];

        let (title_line_spans, title_rects) =
            title_line_with_rects(&title_spans, *TITLE_STYLE, area.x + 1, area.y);
        self.follow_btn_rect = title_rects
            .get(3)
            .copied()
            .map(|r| (r, self.current_channel.clone()));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line_spans))
            .style(*TEXT_DARK_STYLE);

        let mut lines: Vec<Line<'static>> = Vec::new();

        if !*NO_COLOR {
            let p_main = Style::default().fg(purple).add_modifier(Modifier::BOLD);
            let p_bright = Style::default()
                .fg(purple_bright)
                .add_modifier(Modifier::BOLD);
            let p_shadow = Style::default().fg(purple_deep);
            let eye_style = Style::default()
                .fg(Color::Rgb(0xFF, 0xFF, 0xFF))
                .bg(purple)
                .add_modifier(Modifier::BOLD);

            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled("▄██████▄", p_main),
                Span::raw("  "),
                Span::styled("████ █   █ ████", p_bright),
            ]));
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled("██", p_main),
                Span::styled("█", eye_style),
                Span::styled("██", p_main),
                Span::styled("█", eye_style),
                Span::styled("██", p_main),
                Span::raw("  "),
                Span::styled(" ██  █ █ █  ██ ", p_main),
            ]));
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled("▀██", p_main),
                Span::styled("▄", p_shadow),
                Span::styled("▀▀▀", p_main),
                Span::raw("   "),
                Span::styled(" ▀▀   ▀ ▀   ▀▀ ", p_shadow),
            ]));
        }

        if let Some(meta) = &self.snapshot.current_stream {
            let bold_cyan = Style::default().fg(sky_cyan).add_modifier(Modifier::BOLD);
            let label_style = Style::default().fg(text_muted);
            let category_style = Style::default().fg(lavender).add_modifier(Modifier::BOLD);
            let value_style = Style::default().fg(text_bright);

            let game = if meta.game_name.is_empty() {
                "No Category".to_string()
            } else {
                meta.game_name.clone()
            };

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", meta.user_name), bold_cyan),
                Span::styled(format!("· {game}"), category_style),
            ]));

            if meta.is_live {
                let uptime = meta.uptime_text(now_utc);
                lines.push(Line::from(vec![
                    Span::styled(" Viewers: ", label_style),
                    Span::styled(
                        meta.viewers_compact(),
                        Style::default().fg(emerald).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  Uptime: ", label_style),
                    Span::styled(uptime, value_style),
                    Span::styled("  Msgs: ", label_style),
                    Span::styled(msg_count.to_string(), value_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(" Cached Msgs: ", label_style),
                    Span::styled(msg_count.to_string(), value_style),
                ]));
            }

            if !meta.title.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!(" {}", meta.title),
                    Style::default().fg(text_secondary),
                )]));
            }

            if !meta.tags.is_empty() {
                let tags_str = meta
                    .tags
                    .iter()
                    .take(5)
                    .fold(String::new(), |mut acc, t| {
                        let _ = write!(acc, "#{t} ");
                        acc
                    });
                lines.push(Line::from(vec![Span::styled(
                    format!(" {}", tags_str.trim_end()),
                    label_style,
                )]));
            }
        } else {
            lines.push(Line::from(vec![Span::styled(
                format!(" Channel: {}", self.current_channel),
                Style::default()
                    .fg(sky_cyan)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(vec![Span::styled(
                format!(" Cached Msgs: {msg_count}"),
                Style::default().fg(text_muted),
            )]));
        }

        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders live/offline following rows and records mouse hitboxes per row"
    )]
    fn draw_following_list(&mut self, f: &mut Frame, area: Rect, config: &SharedCoreConfig) {
        self.following_rect = Some(area);
        self.channel_hitboxes.clear();
        self.unfollow_hitboxes.clear();
        self.scope_hint_rect = None;

        let emerald = Color::Rgb(0x50, 0xC8, 0x78);
        let sky_cyan = Color::Rgb(0x38, 0xBD, 0xF8);
        let lavender = Color::Rgb(0xA7, 0x8B, 0xFA);
        let text_bright = Color::Rgb(0xEC, 0xF0, 0xFF);
        let text_secondary = Color::Rgb(0xC0, 0xCA, 0xF5);
        let text_offline = Color::Rgb(0xA9, 0xB1, 0xD6);
        let text_muted = Color::Rgb(0x94, 0x9C, 0xBB);

        let now_utc = Utc::now();
        let live_count = self.snapshot.live_channels.len();
        let live_count_str = live_count.to_string();

        let title_spans = [
            TitleStyle::Single("Following"),
            TitleStyle::Combined("Live", &live_count_str),
            TitleStyle::Single("Search"),
        ];

        let (title_line_spans, _title_rects) =
            title_line_with_rects(&title_spans, *TITLE_STYLE, area.x + 1, area.y);
        self.following_header_rect = Some(Rect::new(area.x, area.y, area.width, 1));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line_spans))
            .style(*TEXT_DARK_STYLE);

        let mut items: Vec<ListItem<'static>> = Vec::new();
        let inner_width = usize::from(area.width.saturating_sub(3));
        let inner_x = area.x.saturating_add(1);
        let max_inner_y = area.bottom().saturating_sub(1);
        let mut cur_y = area.y.saturating_add(1);
        let mut skipped = 0usize;

        for stream in &self.snapshot.live_channels {
            if skipped < self.scroll_offset {
                skipped += 1;
                continue;
            }
            let is_active = stream.user_login.eq_ignore_ascii_case(&self.current_channel);
            let name_style = if is_active {
                Style::default().fg(sky_cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(text_bright)
                    .add_modifier(Modifier::BOLD)
            };

            let uptime = stream.uptime_text(now_utc);
            let viewers = stream.viewers_compact();
            let game = if stream.game_name.is_empty() {
                "Live"
            } else {
                stream.game_name.as_str()
            };

            let uptime_str = if uptime.is_empty() {
                String::new()
            } else {
                format!(" · {uptime}")
            };
            let used_chars =
                3 + stream.user_name.chars().count() + 3 + viewers.len() + uptime_str.len() + 3;
            let pad_len = inner_width.saturating_sub(used_chars).max(1);

            let header = Line::from(vec![
                Span::styled(
                    " ● ",
                    Style::default().fg(emerald).add_modifier(Modifier::BOLD),
                ),
                Span::styled(stream.user_name.clone(), name_style),
                Span::styled(
                    format!(" ({viewers})"),
                    Style::default().fg(emerald).add_modifier(Modifier::BOLD),
                ),
                Span::styled(uptime_str, Style::default().fg(text_muted)),
                Span::raw(" ".repeat(pad_len)),
                Span::styled("[×]", Style::default().fg(text_muted)),
            ]);

            let prefix = format!("   {game}");
            let suffix = if stream.title.is_empty() {
                String::new()
            } else {
                format!(" — {}", stream.title)
            };
            let prefix_chars = prefix.chars().count();
            let remaining = inner_width.saturating_sub(prefix_chars);
            let prefix_trunc: String = prefix.chars().take(inner_width).collect();
            let suffix_trunc: String = suffix.chars().take(remaining).collect();

            let subline = Line::from(vec![
                Span::styled(prefix_trunc, Style::default().fg(lavender)),
                Span::styled(suffix_trunc, Style::default().fg(text_secondary)),
            ]);

            if cur_y < max_inner_y {
                let row_h = 2.min(max_inner_y.saturating_sub(cur_y));
                let full_w = area.width.saturating_sub(2);
                let unf_w = 4u16.min(full_w);
                self.unfollow_hitboxes.push((
                    Rect::new(
                        inner_x.saturating_add(full_w.saturating_sub(unf_w)),
                        cur_y,
                        unf_w,
                        1,
                    ),
                    stream.user_login.clone(),
                ));
                self.channel_hitboxes.push((
                    Rect::new(inner_x, cur_y, full_w.saturating_sub(unf_w), row_h),
                    stream.user_login.clone(),
                ));
                cur_y = cur_y.saturating_add(row_h);
            }

            items.push(ListItem::new(Text::from(vec![header, subline])));
        }

        if !self.snapshot.offline_channels.is_empty() {
            for ch in &self.snapshot.offline_channels {
                if skipped < self.scroll_offset {
                    skipped += 1;
                    continue;
                }
                let is_active = ch.eq_ignore_ascii_case(&self.current_channel);
                let style = if is_active {
                    Style::default().fg(sky_cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(text_offline)
                };
                if cur_y < max_inner_y {
                    let full_w = area.width.saturating_sub(2);
                    let unf_w = 4u16.min(full_w);
                    self.unfollow_hitboxes.push((
                        Rect::new(
                            inner_x.saturating_add(full_w.saturating_sub(unf_w)),
                            cur_y,
                            unf_w,
                            1,
                        ),
                        ch.clone(),
                    ));
                    self.channel_hitboxes.push((
                        Rect::new(inner_x, cur_y, full_w.saturating_sub(unf_w), 1),
                        ch.clone(),
                    ));
                    cur_y = cur_y.saturating_add(1);
                }
                let pad_len = inner_width.saturating_sub(ch.chars().count() + 6).max(1);
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(" ○ ", Style::default().fg(text_muted)),
                    Span::styled(ch.clone(), style),
                    Span::raw(" ".repeat(pad_len)),
                    Span::styled("[×]", Style::default().fg(text_muted)),
                ])));
            }
        }

        if self.snapshot.missing_follows_scope {
            if cur_y < max_inner_y {
                let row_h = 2.min(max_inner_y.saturating_sub(cur_y));
                self.scope_hint_rect =
                    Some(Rect::new(inner_x, cur_y, area.width.saturating_sub(2), row_h));
            }
            items.push(ListItem::new(Line::from(vec![Span::styled(
                " ℹ Add user:read:follows scope",
                Style::default().fg(sky_cyan).add_modifier(Modifier::BOLD),
            )])));
            items.push(ListItem::new(Line::from(vec![Span::styled(
                "   (click to generate token)",
                Style::default()
                    .fg(text_secondary)
                    .add_modifier(Modifier::UNDERLINED),
            )])));
        }

        let list = List::new(items).block(block);
        f.render_widget(list, area);
    }
}

fn parse_helix_stream(item: HelixStreamItem) -> StreamMetadata {
    let started_at = item
        .started_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    StreamMetadata {
        user_login: item.user_login.to_lowercase(),
        user_name: item.user_name,
        game_name: item.game_name,
        title: item.title,
        viewer_count: item.viewer_count,
        started_at,
        tags: item.tags,
        is_live: true,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Queries followed streams, followed channels, global top streams, and top categories in one snapshot pass"
)]
async fn fetch_sidebar_snapshot(
    client: &Client,
    active_channel: &str,
    tracked_channels: Vec<String>,
    unfollowed_channels: Vec<String>,
    user_id: Option<&str>,
    has_follows_scope: bool,
) -> SidebarSnapshot {
    let mut live_map: HashMap<String, StreamMetadata> = HashMap::new();
    let mut all_known_logins: BTreeSet<String> = BTreeSet::new();
    let unfollowed_set: BTreeSet<String> = unfollowed_channels
        .into_iter()
        .map(|ch| ch.trim().trim_start_matches('#').to_lowercase())
        .filter(|ch| !ch.is_empty())
        .collect();

    for ch in tracked_channels {
        let clean = ch.trim().trim_start_matches('#').to_lowercase();
        if !clean.is_empty() && !unfollowed_set.contains(&clean) {
            all_known_logins.insert(clean);
        }
    }

    let mut missing_follows_scope = !has_follows_scope;

    if has_follows_scope {
        if let Some(uid) = user_id {
            let live_url =
                format!("{TWITCH_API_BASE_URL}/streams/followed?user_id={uid}&first=100");
            if let Ok(resp) = client.get(live_url).send().await {
                if resp.status().is_success() {
                    if let Ok(list) = resp.json::<HelixStreamList>().await {
                        for item in list.data {
                            let meta = parse_helix_stream(item);
                            if !unfollowed_set.contains(&meta.user_login) {
                                all_known_logins.insert(meta.user_login.clone());
                                live_map.insert(meta.user_login.clone(), meta);
                            }
                        }
                    }
                } else if resp.status().as_u16() == 401 {
                    missing_follows_scope = true;
                }
            }

            let mut follows_cursor: Option<String> = None;
            for _ in 0..5 {
                let mut all_follows_url =
                    format!("{TWITCH_API_BASE_URL}/channels/followed?user_id={uid}&first=100");
                if let Some(cur) = &follows_cursor {
                    let _ = write!(all_follows_url, "&after={cur}");
                }
                let Ok(resp) = client.get(all_follows_url).send().await else {
                    break;
                };
                let Ok(list) = resp.json::<HelixFollowedChannelList>().await else {
                    break;
                };
                for item in list.data {
                    let login = item.broadcaster_login.to_lowercase();
                    if !unfollowed_set.contains(&login) {
                        all_known_logins.insert(login);
                    }
                }
                if list.pagination.cursor.is_none() {
                    break;
                }
                follows_cursor = list.pagination.cursor;
            }
        }
    }

    // Also query `/helix/streams?user_login=...` for any followed channels not yet in `live_map`
    let logins_to_query: Vec<String> = all_known_logins
        .iter()
        .filter(|login| !live_map.contains_key(*login))
        .take(90)
        .cloned()
        .collect();

    if !logins_to_query.is_empty() {
        let mut query_url = format!("{TWITCH_API_BASE_URL}/streams?");
        for (idx, login) in logins_to_query.iter().enumerate() {
            if idx > 0 {
                query_url.push('&');
            }
            let _ = write!(query_url, "user_login={login}");
        }

        if let Ok(resp) = client.get(query_url).send().await {
            if let Ok(list) = resp.json::<HelixStreamList>().await {
                for item in list.data {
                    let meta = parse_helix_stream(item);
                    if !unfollowed_set.contains(&meta.user_login) {
                        live_map.insert(meta.user_login.clone(), meta);
                    }
                }
            }
        }
    }

    let current_lower = active_channel.to_lowercase();
    let current_stream = if let Some(live_meta) = live_map.get(&current_lower) {
        Some(live_meta.clone())
    } else if !current_lower.is_empty() {
        let stream_url = format!("{TWITCH_API_BASE_URL}/streams?user_login={current_lower}");
        let mut fetched_live = None;
        if let Ok(resp) = client.get(stream_url).send().await {
            if let Ok(list) = resp.json::<HelixStreamList>().await {
                if let Some(item) = list.data.into_iter().next() {
                    fetched_live = Some(parse_helix_stream(item));
                }
            }
        }
        if fetched_live.is_some() {
            fetched_live
        } else {
            fetch_offline_channel_metadata(client, &current_lower).await
        }
    } else {
        None
    };

    let mut live_channels: Vec<StreamMetadata> = live_map.into_values().collect();
    live_channels.sort_by_key(|a| std::cmp::Reverse(a.viewer_count));

    let offline_channels: Vec<String> = all_known_logins
        .into_iter()
        .filter(|login| {
            !live_channels
                .iter()
                .any(|live| live.user_login.eq_ignore_ascii_case(login))
        })
        .collect();

    let mut top_streams: Vec<StreamMetadata> = Vec::new();
    let mut top_streams_cursor: Option<String> = None;
    let top_streams_url = format!("{TWITCH_API_BASE_URL}/streams?first=36");
    if let Ok(resp) = client.get(top_streams_url).send().await {
        if let Ok(list) = resp.json::<HelixStreamList>().await {
            top_streams_cursor = list.pagination.cursor;
            for item in list.data {
                top_streams.push(parse_helix_stream(item));
            }
        }
    }

    let mut top_categories: Vec<CategoryMetadata> = Vec::new();
    let top_games_url = format!("{TWITCH_API_BASE_URL}/games/top?first=20");
    if let Ok(resp) = client.get(top_games_url).send().await {
        if let Ok(list) = resp.json::<HelixGameList>().await {
            for g in list.data {
                top_categories.push(CategoryMetadata {
                    id: g.id,
                    name: g.name,
                });
            }
        }
    }
    if !top_categories
        .iter()
        .any(|c| c.id == "509659" || c.name.eq_ignore_ascii_case("ASMR"))
    {
        top_categories.insert(
            0,
            CategoryMetadata {
                id: "509659".to_string(),
                name: "ASMR".to_string(),
            },
        );
    }

    SidebarSnapshot {
        current_stream,
        live_channels,
        offline_channels,
        top_streams,
        top_streams_cursor,
        top_categories,
        missing_follows_scope,
    }
}

pub async fn fetch_top_streams_page(
    twitch_oauth: &TwitchOauth,
    after: Option<&str>,
) -> (Vec<StreamMetadata>, Option<String>) {
    let Some(client) = twitch_oauth.client() else {
        return (Vec::new(), None);
    };
    let mut url = format!("{TWITCH_API_BASE_URL}/streams?first=36");
    if let Some(cur) = after {
        let _ = write!(url, "&after={cur}");
    }
    let Ok(resp) = client.get(url).send().await else {
        return (Vec::new(), None);
    };
    let Ok(list) = resp.json::<HelixStreamList>().await else {
        return (Vec::new(), None);
    };
    let streams = list.data.into_iter().map(parse_helix_stream).collect();
    (streams, list.pagination.cursor)
}

pub async fn fetch_category_streams_page(
    twitch_oauth: &TwitchOauth,
    game_id: &str,
    after: Option<&str>,
) -> (Vec<StreamMetadata>, Option<String>) {
    let Some(client) = twitch_oauth.client() else {
        return (Vec::new(), None);
    };
    let mut url = format!("{TWITCH_API_BASE_URL}/streams?game_id={game_id}&first=36");
    if let Some(cur) = after {
        let _ = write!(url, "&after={cur}");
    }
    let Ok(resp) = client.get(url).send().await else {
        return (Vec::new(), None);
    };
    let Ok(list) = resp.json::<HelixStreamList>().await else {
        return (Vec::new(), None);
    };
    let streams = list.data.into_iter().map(parse_helix_stream).collect();
    (streams, list.pagination.cursor)
}

pub async fn search_live_channels_page(
    twitch_oauth: &TwitchOauth,
    query: &str,
    after: Option<&str>,
) -> (Vec<StreamMetadata>, Option<String>) {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return (Vec::new(), None);
    }
    let Some(client) = twitch_oauth.client() else {
        return (Vec::new(), None);
    };
    let mut req = client
        .get(format!("{TWITCH_API_BASE_URL}/search/channels"))
        .query(&[
            ("query", trimmed),
            ("live_only", "true"),
            ("first", "36"),
        ]);
    if let Some(cur) = after {
        req = req.query(&[("after", cur)]);
    }
    let Ok(resp) = req.send().await else {
        return (Vec::new(), None);
    };
    let Ok(search_list) = resp.json::<HelixSearchChannelList>().await else {
        return (Vec::new(), None);
    };
    if search_list.data.is_empty() {
        return (Vec::new(), None);
    }
    let next_cursor = search_list.pagination.cursor;

    let logins: Vec<String> = search_list
        .data
        .iter()
        .map(|item| item.broadcaster_login.to_lowercase())
        .take(36)
        .collect();

    let mut live_by_login: HashMap<String, StreamMetadata> = HashMap::new();
    let mut stream_url = format!("{TWITCH_API_BASE_URL}/streams?");
    for (idx, login) in logins.iter().enumerate() {
        if idx > 0 {
            stream_url.push('&');
        }
        let _ = write!(stream_url, "user_login={login}");
    }
    if let Ok(s_resp) = client.get(stream_url).send().await {
        if let Ok(s_list) = s_resp.json::<HelixStreamList>().await {
            for item in s_list.data {
                let meta = parse_helix_stream(item);
                live_by_login.insert(meta.user_login.clone(), meta);
            }
        }
    }

    let mut out = Vec::new();
    for item in search_list.data {
        let login = item.broadcaster_login.to_lowercase();
        if let Some(meta) = live_by_login.remove(&login) {
            out.push(meta);
        } else if item.is_live {
            let started_at = item
                .started_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            out.push(StreamMetadata {
                user_login: login,
                user_name: item.display_name,
                game_name: item.game_name,
                title: item.title,
                viewer_count: 0,
                started_at,
                tags: item.tags,
                is_live: true,
            });
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.viewer_count));
    (out, next_cursor)
}

async fn fetch_offline_channel_metadata(
    client: &Client,
    channel_login: &str,
) -> Option<StreamMetadata> {
    let user_url = format!("{TWITCH_API_BASE_URL}/users?login={channel_login}");
    let user_resp = client.get(user_url).send().await.ok()?;
    let user_list = user_resp.json::<HelixUserList>().await.ok()?;
    let user = user_list.data.first()?;

    let chan_url = format!("{TWITCH_API_BASE_URL}/channels?broadcaster_id={}", user.id);
    if let Ok(chan_resp) = client.get(chan_url).send().await {
        if let Ok(chan_list) = chan_resp.json::<HelixChannelInfoList>().await {
            if let Some(info) = chan_list.data.first() {
                return Some(StreamMetadata {
                    user_login: info.broadcaster_login.to_lowercase(),
                    user_name: info.broadcaster_name.clone(),
                    game_name: info.game_name.clone(),
                    title: info.title.clone(),
                    viewer_count: 0,
                    started_at: None,
                    tags: info.tags.clone(),
                    is_live: false,
                });
            }
        }
    }

    Some(StreamMetadata {
        user_login: user.login.to_lowercase(),
        user_name: user.display_name.clone(),
        game_name: String::new(),
        title: String::new(),
        viewer_count: 0,
        started_at: None,
        tags: Vec::new(),
        is_live: false,
    })
}

pub async fn is_channel_live(twitch_oauth: &TwitchOauth, channel: &str) -> bool {
    let clean = channel.trim().trim_start_matches('#').to_lowercase();
    if clean.is_empty() {
        return false;
    }
    let Some(client) = twitch_oauth.client() else {
        return false;
    };
    let url = format!("{TWITCH_API_BASE_URL}/streams?user_login={clean}");
    let Ok(resp) = client
        .get(url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
    else {
        return false;
    };
    let Ok(list) = resp.json::<HelixStreamList>().await else {
        return false;
    };
    !list.data.is_empty()
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    #[test]
    fn test_stream_metadata_formatting() {
        let now = Utc::now();
        let meta = StreamMetadata {
            user_login: "bowmandean".to_string(),
            user_name: "BowmanDean".to_string(),
            game_name: "ASMR".to_string(),
            title: "Test stream".to_string(),
            viewer_count: 1450,
            started_at: Some(now - TimeDelta::minutes(85)),
            tags: vec!["Cozy".to_string()],
            is_live: true,
        };

        assert_eq!(meta.viewers_compact(), "1.4k");
        assert_eq!(meta.uptime_text(now), "1h25m");
    }
}
