use chrono::Utc;
use color_eyre::Result;
use tokio::sync::mpsc::{self, Sender, UnboundedReceiver, UnboundedSender};
use tui::{
    Frame,
    crossterm::event::{MouseButton, MouseEventKind},
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::SharedCoreConfig,
    events::{Event, InternalEvent, Key, TwitchAction, TwitchEvent, get_keybind_text},
    handlers::{
        sidebar::{
            CategoryMetadata, SharedSidebar, StreamMetadata, fetch_category_streams_page,
            fetch_top_streams_page, search_live_channels_page,
        },
        state::State,
        storage::SharedStorage,
    },
    twitch::oauth::TwitchOauth,
    ui::components::{ChannelSwitcherWidget, Component, FollowingWidget, utils::InputWidget},
    utils::{
        styles::{BORDER_DARK_STYLE, NO_COLOR, TEXT_DARK_STYLE, TITLE_STYLE},
        text::{TitleStyle, title_line},
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowseTab {
    #[default]
    Following,
    TopLive,
    Categories,
    Search,
}

#[derive(Debug)]
enum BrowseAsyncResult {
    TopStreams {
        streams: Vec<StreamMetadata>,
        cursor: Option<String>,
        append: bool,
    },
    CategoryStreams {
        game_id: String,
        streams: Vec<StreamMetadata>,
        cursor: Option<String>,
        append: bool,
    },
    SearchResults {
        query: String,
        streams: Vec<StreamMetadata>,
        cursor: Option<String>,
        append: bool,
    },
}

pub struct DashboardWidget {
    config: SharedCoreConfig,
    twitch_oauth: TwitchOauth,
    event_tx: Sender<Event>,
    storage: SharedStorage,
    sidebar: SharedSidebar,
    channel_input: ChannelSwitcherWidget,
    following: FollowingWidget,
    browse_tab: BrowseTab,
    top_loading_more: bool,
    selected_category: Option<CategoryMetadata>,
    category_streams: Vec<StreamMetadata>,
    category_cursor: Option<String>,
    category_loading: bool,
    search_input: InputWidget<()>,
    search_query: String,
    search_results: Vec<StreamMetadata>,
    search_cursor: Option<String>,
    search_loading: bool,
    browse_tx: UnboundedSender<BrowseAsyncResult>,
    browse_rx: UnboundedReceiver<BrowseAsyncResult>,
    selected_idx: usize,
    scroll_offset: usize,
    card_scroll_offset: usize,
    last_grid_cols: usize,
    last_grid_rows: usize,
    sidebar_hitboxes: Vec<(Rect, String)>,
    unfollow_hitboxes: Vec<(Rect, String)>,
    card_hitboxes: Vec<(Rect, String)>,
    category_hitboxes: Vec<(Rect, CategoryMetadata)>,
    chip_hitboxes: Vec<(Rect, String)>,
    tab_rects: Vec<(Rect, BrowseTab)>,
    action_rects: Vec<Rect>,
    sidebar_area: Option<Rect>,
    grid_area: Option<Rect>,
    search_bar_rect: Option<Rect>,
}

impl DashboardWidget {
    pub fn new(
        config: SharedCoreConfig,
        twitch_oauth: TwitchOauth,
        event_tx: Sender<Event>,
        storage: SharedStorage,
        sidebar: SharedSidebar,
    ) -> Self {
        let channel_input =
            ChannelSwitcherWidget::new(config.clone(), event_tx.clone(), storage.clone());
        let following = FollowingWidget::new(
            config.clone(),
            twitch_oauth.clone(),
            event_tx.clone(),
            storage.clone(),
        );
        let search_input = InputWidget::builder()
            .config(config.clone())
            .event_tx(event_tx.clone())
            .title("Search Live Twitch Channels (Enter to search, Esc to unfocus)")
            .build();
        let (browse_tx, browse_rx) = mpsc::unbounded_channel();

        Self {
            config,
            twitch_oauth,
            event_tx,
            storage,
            sidebar,
            channel_input,
            following,
            browse_tab: BrowseTab::Following,
            top_loading_more: false,
            selected_category: None,
            category_streams: Vec::new(),
            category_cursor: None,
            category_loading: false,
            search_input,
            search_query: String::new(),
            search_results: Vec::new(),
            search_cursor: None,
            search_loading: false,
            browse_tx,
            browse_rx,
            selected_idx: 0,
            scroll_offset: 0,
            card_scroll_offset: 0,
            last_grid_cols: 3,
            last_grid_rows: 3,
            sidebar_hitboxes: Vec::new(),
            unfollow_hitboxes: Vec::new(),
            card_hitboxes: Vec::new(),
            category_hitboxes: Vec::new(),
            chip_hitboxes: Vec::new(),
            tab_rects: Vec::new(),
            action_rects: Vec::new(),
            sidebar_area: None,
            grid_area: None,
            search_bar_rect: None,
        }
    }

    pub fn poll_updates(&mut self) -> bool {
        let mut updated = false;
        while let Ok(msg) = self.browse_rx.try_recv() {
            match msg {
                BrowseAsyncResult::TopStreams {
                    streams,
                    cursor,
                    append,
                } => {
                    self.top_loading_more = false;
                    let mut sb = self.sidebar.borrow_mut();
                    sb.snapshot.top_streams_cursor = cursor;
                    if append {
                        for s in streams {
                            if !sb
                                .snapshot
                                .top_streams
                                .iter()
                                .any(|existing| existing.user_login.eq_ignore_ascii_case(&s.user_login))
                            {
                                sb.snapshot.top_streams.push(s);
                            }
                        }
                    } else {
                        sb.snapshot.top_streams = streams;
                    }
                    updated = true;
                }
                BrowseAsyncResult::CategoryStreams {
                    game_id,
                    streams,
                    cursor,
                    append,
                } => {
                    if self
                        .selected_category
                        .as_ref()
                        .is_some_and(|c| c.id == game_id)
                    {
                        self.category_loading = false;
                        self.category_cursor = cursor;
                        if append {
                            for s in streams {
                                if !self
                                    .category_streams
                                    .iter()
                                    .any(|existing| existing.user_login.eq_ignore_ascii_case(&s.user_login))
                                {
                                    self.category_streams.push(s);
                                }
                            }
                        } else {
                            self.category_streams = streams;
                            self.selected_idx = 0;
                            self.card_scroll_offset = 0;
                        }
                        updated = true;
                    }
                }
                BrowseAsyncResult::SearchResults {
                    query,
                    streams,
                    cursor,
                    append,
                } => {
                    if self.search_query == query {
                        self.search_loading = false;
                        self.search_cursor = cursor;
                        if append {
                            for s in streams {
                                if !self
                                    .search_results
                                    .iter()
                                    .any(|existing| existing.user_login.eq_ignore_ascii_case(&s.user_login))
                                {
                                    self.search_results.push(s);
                                }
                            }
                        } else {
                            self.search_results = streams;
                            self.selected_idx = 0;
                            self.card_scroll_offset = 0;
                        }
                        updated = true;
                    }
                }
            }
        }
        updated
    }

    fn select_category(&mut self, cat: CategoryMetadata) {
        let game_id = cat.id.clone();
        self.selected_category = Some(cat);
        self.category_streams.clear();
        self.category_cursor = None;
        self.category_loading = true;
        self.card_scroll_offset = 0;
        self.selected_idx = 0;

        let oauth = self.twitch_oauth.clone();
        let tx = self.browse_tx.clone();
        tokio::spawn(async move {
            let (streams, cursor) = fetch_category_streams_page(&oauth, &game_id, None).await;
            let _ = tx.send(BrowseAsyncResult::CategoryStreams {
                game_id,
                streams,
                cursor,
                append: false,
            });
        });
    }

    fn trigger_live_search(&mut self, query: &str) {
        let clean = query.trim().to_string();
        if clean.is_empty() {
            return;
        }
        self.search_query.clone_from(&clean);
        self.search_results.clear();
        self.search_cursor = None;
        self.search_loading = true;
        self.card_scroll_offset = 0;
        self.selected_idx = 0;
        self.browse_tab = BrowseTab::Search;

        let oauth = self.twitch_oauth.clone();
        let tx = self.browse_tx.clone();
        tokio::spawn(async move {
            let (streams, cursor) = search_live_channels_page(&oauth, &clean, None).await;
            let _ = tx.send(BrowseAsyncResult::SearchResults {
                query: clean,
                streams,
                cursor,
                append: false,
            });
        });
    }

    fn maybe_load_more(&mut self) {
        let snap_live_empty = self.sidebar.borrow().snapshot.live_channels.is_empty();
        let snap_top_cursor = self.sidebar.borrow().snapshot.top_streams_cursor.clone();

        match self.browse_tab {
            BrowseTab::TopLive => {
                if !self.top_loading_more {
                    if let Some(cur) = snap_top_cursor {
                        self.top_loading_more = true;
                        let oauth = self.twitch_oauth.clone();
                        let tx = self.browse_tx.clone();
                        tokio::spawn(async move {
                            let (streams, cursor) =
                                fetch_top_streams_page(&oauth, Some(&cur)).await;
                            let _ = tx.send(BrowseAsyncResult::TopStreams {
                                streams,
                                cursor,
                                append: true,
                            });
                        });
                    }
                }
            }
            BrowseTab::Following if snap_live_empty => {
                if !self.top_loading_more {
                    if let Some(cur) = snap_top_cursor {
                        self.top_loading_more = true;
                        let oauth = self.twitch_oauth.clone();
                        let tx = self.browse_tx.clone();
                        tokio::spawn(async move {
                            let (streams, cursor) =
                                fetch_top_streams_page(&oauth, Some(&cur)).await;
                            let _ = tx.send(BrowseAsyncResult::TopStreams {
                                streams,
                                cursor,
                                append: true,
                            });
                        });
                    }
                }
            }
            BrowseTab::Categories => {
                if let Some(cat) = self.selected_category.clone() {
                    if !self.category_loading {
                        if let Some(cur) = self.category_cursor.clone() {
                            self.category_loading = true;
                            let game_id = cat.id;
                            let oauth = self.twitch_oauth.clone();
                            let tx = self.browse_tx.clone();
                            tokio::spawn(async move {
                                let (streams, cursor) =
                                    fetch_category_streams_page(&oauth, &game_id, Some(&cur)).await;
                                let _ = tx.send(BrowseAsyncResult::CategoryStreams {
                                    game_id,
                                    streams,
                                    cursor,
                                    append: true,
                                });
                            });
                        }
                    }
                } else if !self.top_loading_more {
                    if let Some(cur) = snap_top_cursor {
                        self.top_loading_more = true;
                        let oauth = self.twitch_oauth.clone();
                        let tx = self.browse_tx.clone();
                        tokio::spawn(async move {
                            let (streams, cursor) =
                                fetch_top_streams_page(&oauth, Some(&cur)).await;
                            let _ = tx.send(BrowseAsyncResult::TopStreams {
                                streams,
                                cursor,
                                append: true,
                            });
                        });
                    }
                }
            }
            BrowseTab::Search => {
                if !self.search_loading && !self.search_query.is_empty() {
                    if let Some(cur) = self.search_cursor.clone() {
                        self.search_loading = true;
                        let query = self.search_query.clone();
                        let oauth = self.twitch_oauth.clone();
                        let tx = self.browse_tx.clone();
                        tokio::spawn(async move {
                            let (streams, cursor) =
                                search_live_channels_page(&oauth, &query, Some(&cur)).await;
                            let _ = tx.send(BrowseAsyncResult::SearchResults {
                                query,
                                streams,
                                cursor,
                                append: true,
                            });
                        });
                    }
                }
            }
            BrowseTab::Following => {}
        }
    }

    fn sync_card_scroll_to_selection(&mut self) {
        let cols = self.last_grid_cols.max(1);
        let rows_visible = self.last_grid_rows.max(1);
        let grid_len = self.active_grid_streams().len();
        if self.selected_idx >= grid_len || grid_len == 0 {
            return;
        }
        let selected_row = self.selected_idx / cols;
        if selected_row < self.card_scroll_offset {
            self.card_scroll_offset = selected_row;
        } else if selected_row >= self.card_scroll_offset + rows_visible {
            self.card_scroll_offset = selected_row.saturating_sub(rows_visible - 1);
        }
        let total_rows = grid_len.div_ceil(cols);
        if self.card_scroll_offset + rows_visible + 1 >= total_rows {
            self.maybe_load_more();
        }
    }

    fn scroll_grid_down(&mut self) {
        let cols = self.last_grid_cols.max(1);
        let rows_visible = self.last_grid_rows.max(1);
        let grid_len = self.active_grid_streams().len();
        let total_rows = grid_len.div_ceil(cols);
        let max_start_row = total_rows.saturating_sub(rows_visible);
        if self.card_scroll_offset < max_start_row {
            self.card_scroll_offset += 1;
        }
        if self.card_scroll_offset + rows_visible + 1 >= total_rows {
            self.maybe_load_more();
        }
    }

    fn active_grid_streams(&self) -> Vec<StreamMetadata> {
        let snap = &self.sidebar.borrow().snapshot;
        match self.browse_tab {
            BrowseTab::Following => {
                if snap.live_channels.is_empty() {
                    snap.top_streams.clone()
                } else {
                    snap.live_channels.clone()
                }
            }
            BrowseTab::TopLive => snap.top_streams.clone(),
            BrowseTab::Categories => {
                if self.selected_category.is_some() {
                    self.category_streams.clone()
                } else {
                    snap.top_streams.clone()
                }
            }
            BrowseTab::Search => self.search_results.clone(),
        }
    }

    fn all_selectable_channels(&self) -> Vec<String> {
        let snap = &self.sidebar.borrow().snapshot;
        let mut out: Vec<String> = Vec::new();

        for s in self.active_grid_streams() {
            if !out.iter().any(|c| c.eq_ignore_ascii_case(&s.user_login)) {
                out.push(s.user_login);
            }
        }

        for s in &snap.live_channels {
            if !out.iter().any(|c| c.eq_ignore_ascii_case(&s.user_login)) {
                out.push(s.user_login.clone());
            }
        }

        for fav in &self.config.frontend.favorite_channels {
            let clean = fav.trim().trim_start_matches('#').to_lowercase();
            if !clean.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(&clean)) {
                out.push(clean);
            }
        }

        if let Ok(recents) = self.storage.borrow().get_last_n("channels", 8, true) {
            for r in recents {
                let clean = r.trim().trim_start_matches('#').to_lowercase();
                if !clean.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(&clean)) {
                    out.push(clean);
                }
            }
        }

        for off in &snap.offline_channels {
            if !out.iter().any(|c| c.eq_ignore_ascii_case(off)) {
                out.push(off.clone());
            }
        }

        if out.is_empty() {
            let last = self
                .storage
                .borrow()
                .last_channel()
                .unwrap_or_else(|| self.config.twitch.channel.clone());
            out.push(last);
        }

        out
    }

    fn toggle_follow(&self, channel: &str) {
        let clean = channel.trim().trim_start_matches('#').to_lowercase();
        if clean.is_empty() {
            return;
        }
        let is_followed = self.sidebar.borrow().is_followed(&clean);
        if is_followed {
            self.storage.borrow_mut().unfollow_channel(&clean);
            self.sidebar.borrow_mut().remove_followed_immediately(&clean);
        } else {
            self.storage.borrow_mut().follow_channel(&clean);
            self.sidebar.borrow_mut().add_followed_immediately(&clean);
        }
    }

    async fn join_channel(&self, channel: &str) -> Result<()> {
        let clean = channel.trim().trim_start_matches('#').to_string();
        if clean.is_empty() {
            return Ok(());
        }
        self.storage.borrow_mut().set_last_channel(&clean);
        self.event_tx
            .send(Event::Twitch(TwitchEvent::Action(
                TwitchAction::JoinChannel(clean),
            )))
            .await?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders the large AGY-style 5-row truecolor Twitch Glitch + TWT block-art logo alongside interactive Browse tabs and action pills"
    )]
    fn draw_top_navbar(&mut self, f: &mut Frame, area: Rect) {
        let purple = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x91, 0x46, 0xFF)
        };
        let purple_bright = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xA9, 0x70, 0xFF)
        };
        let purple_deep = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x5C, 0x16, 0xC5)
        };
        let white = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xFF, 0xFF, 0xFF)
        };
        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };
        let green = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x50, 0xC8, 0x78)
        };
        let amber = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xF5, 0x9E, 0x0B)
        };
        let cyan = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x38, 0xBD, 0xF8)
        };

        let last_chan = self
            .storage
            .borrow()
            .last_channel()
            .unwrap_or_else(|| self.config.twitch.channel.clone());
        let is_last_live = self
            .sidebar
            .borrow()
            .snapshot
            .live_channels
            .iter()
            .any(|s| s.user_login.eq_ignore_ascii_case(&last_chan));

        let keybinds = &self.config.keybinds.dashboard;
        let actions = [
            (
                get_keybind_text(&keybinds.followed_channels_search),
                "Following",
            ),
            (get_keybind_text(&keybinds.recent_channels_search), "Channel"),
            ("r".to_string(), "Refresh"),
            (get_keybind_text(&keybinds.help), "Help"),
            (get_keybind_text(&keybinds.quit), "Quit"),
        ];

        self.tab_rects.clear();
        self.action_rects.clear();

        let p_main = Style::default().fg(purple).add_modifier(Modifier::BOLD);
        let p_bright = Style::default()
            .fg(purple_bright)
            .add_modifier(Modifier::BOLD);
        let p_shadow = Style::default().fg(purple_deep);
        let eye_style = Style::default()
            .fg(white)
            .bg(purple)
            .add_modifier(Modifier::BOLD);

        // Row 1: Large Twitch Glitch mascot top + TWT block-art top + Interactive Browse tabs + Action pills
        let mut row1_spans = vec![
            Span::raw("  "),
            Span::styled("▄████████████▄", p_main),
            Span::raw("   "),
            Span::styled("██████  ██    ██  ██████", p_bright),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
        ];

        let mut x = area.x.saturating_add(
            u16::try_from(
                row1_spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum::<usize>(),
            )
            .unwrap_or(50)
            .saturating_add(1),
        );
        let row1_y = area.y.saturating_add(1);

        let browse_tabs = [
            (BrowseTab::Following, "Following"),
            (BrowseTab::TopLive, "Top Live"),
            (BrowseTab::Categories, "Categories"),
            (BrowseTab::Search, "Search (/)"),
        ];

        for (i, (tab, label)) in browse_tabs.iter().enumerate() {
            if i > 0 {
                row1_spans.push(Span::styled(" · ", Style::default().fg(text_secondary)));
                x = x.saturating_add(3);
            }
            let active = &self.browse_tab == tab;
            let pill = format!(" {label} ");
            let w = u16::try_from(UnicodeWidthStr::width(pill.as_str())).unwrap_or(8);
            if x.saturating_add(w) < area.right() {
                self.tab_rects.push((Rect::new(x, row1_y, w, 1), tab.clone()));
            }
            x = x.saturating_add(w);
            let style = if active {
                Style::default()
                    .fg(cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(text_secondary)
            };
            row1_spans.push(Span::styled(pill, style));
        }

        row1_spans.push(Span::styled("  │  ", Style::default().fg(text_secondary)));
        x = x.saturating_add(5);

        for (i, (key_txt, label)) in actions.iter().enumerate() {
            if i > 0 {
                row1_spans.push(Span::raw("  "));
                x = x.saturating_add(2);
            }
            let pill_str = format!("[{key_txt}] {label}");
            let w = u16::try_from(UnicodeWidthStr::width(pill_str.as_str())).unwrap_or(10);
            if x.saturating_add(w) < area.right() {
                self.action_rects.push(Rect::new(x, row1_y, w, 1));
            }
            x = x.saturating_add(w);
            row1_spans.push(Span::styled("[", Style::default().fg(text_secondary)));
            row1_spans.push(Span::styled(key_txt.clone(), p_bright));
            row1_spans.push(Span::styled("] ", Style::default().fg(text_secondary)));
            row1_spans.push(Span::styled(*label, Style::default().fg(text_primary)));
        }

        // Row 2: Glitch mascot upper eyes + TWT block-art row 2 + Account / version info
        let row2_spans = vec![
            Span::raw("  "),
            Span::styled("███", p_main),
            Span::styled("██", eye_style),
            Span::styled("████", p_main),
            Span::styled("██", eye_style),
            Span::styled("███", p_main),
            Span::styled("█", p_shadow),
            Span::raw("  "),
            Span::styled("  ██    ██    ██    ██  ", p_bright),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled(" Twitch TUI ", p_bright),
            Span::styled(
                format!("v{}  •  Signed in as ", env!("CARGO_PKG_VERSION")),
                Style::default().fg(text_secondary),
            ),
            Span::styled(
                format!("@{}", self.config.twitch.username),
                Style::default()
                    .fg(text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        // Row 3: Glitch mascot lower eyes + TWT block-art row 3 + Last watched channel status
        let row3_spans = vec![
            Span::raw("  "),
            Span::styled("███", p_main),
            Span::styled("██", eye_style),
            Span::styled("████", p_main),
            Span::styled("██", eye_style),
            Span::styled("███", p_main),
            Span::styled("█", p_shadow),
            Span::raw("  "),
            Span::styled("  ██    ██ █▄ █     ██  ", p_main),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled("Last watched: ", Style::default().fg(text_secondary)),
            Span::styled(
                format!("#{last_chan} "),
                Style::default()
                    .fg(text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            if is_last_live {
                Span::styled(
                    "● LIVE (Press Enter to join)",
                    Style::default().fg(green).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    "○ OFFLINE — Browse live streams & categories below",
                    Style::default().fg(amber),
                )
            },
        ];

        // Row 4: Glitch mascot lower jaw + TWT block-art row 4 + Live followed & global stream counts
        let snap = &self.sidebar.borrow().snapshot;
        let live_count = snap.live_channels.len();
        let global_count = snap.top_streams.len();
        let cat_count = snap.top_categories.len();
        let row4_spans = vec![
            Span::raw("  "),
            Span::styled("████████████▀▀", p_main),
            Span::styled("█", p_shadow),
            Span::raw("  "),
            Span::styled("  ██    ██▄██▄██    ██  ", p_main),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled(
                format!("● {live_count} followed live"),
                Style::default().fg(if live_count > 0 {
                    green
                } else {
                    text_secondary
                }),
            ),
            Span::styled(
                format!("  •  🌐 {global_count} top global streams  •  🏷 {cat_count} categories"),
                Style::default().fg(text_secondary),
            ),
        ];

        // Row 5: Glitch mascot speech-bubble tail + TWT block-art base + navigation hint
        let row5_spans = vec![
            Span::raw("    "),
            Span::styled("▀██▀", p_main),
            Span::styled("▄▄▀▀▀▀▀", p_shadow),
            Span::raw("    "),
            Span::styled("  ▀▀     ▀▀  ▀▀     ▀▀  ", p_shadow),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled("Press ", Style::default().fg(text_secondary)),
            Span::styled("Tab", p_bright),
            Span::styled(" to switch views, ", Style::default().fg(text_secondary)),
            Span::styled("/", p_bright),
            Span::styled(" to search, ", Style::default().fg(text_secondary)),
            Span::styled("1-9", p_bright),
            Span::styled("/", Style::default().fg(text_secondary)),
            Span::styled("Enter", p_bright),
            Span::styled(" to watch, hold ", Style::default().fg(text_secondary)),
            Span::styled("`", p_bright),
            Span::styled(" or ", Style::default().fg(text_secondary)),
            Span::styled("Space", p_bright),
            Span::styled(" for keybind overlay", Style::default().fg(text_secondary)),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE);

        let lines = if area.height >= 7 {
            vec![
                Line::from(row1_spans),
                Line::from(row2_spans),
                Line::from(row3_spans),
                Line::from(row4_spans),
                Line::from(row5_spans),
            ]
        } else if area.height >= 5 {
            vec![
                Line::from(row1_spans),
                Line::from(row2_spans),
                Line::from(row3_spans),
            ]
        } else {
            vec![Line::from(row1_spans)]
        };

        let nav = Paragraph::new(lines).block(block);
        f.render_widget(nav, area);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders the left Twitch-style Followed Channels directory with live and offline sections"
    )]
    fn draw_left_sidebar(&mut self, f: &mut Frame, area: Rect, all_channels: &[String]) {
        self.sidebar_area = Some(area);
        self.sidebar_hitboxes.clear();
        self.unfollow_hitboxes.clear();

        let snap = self.sidebar.borrow().snapshot.clone();
        let live_count = snap.live_channels.len();
        let total_followed = live_count + snap.offline_channels.len();

        let title_str = format!("{live_count} live / {total_followed}");
        let header_spans = [
            TitleStyle::Combined("FOLLOWED CHANNELS", &title_str),
            TitleStyle::Single("u / [×] Unfollow"),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line(&header_spans, *TITLE_STYLE)))
            .style(*TEXT_DARK_STYLE);

        let inner_w = usize::from(area.width.saturating_sub(3)).max(12);
        let max_rows = usize::from(area.height.saturating_sub(2));
        let inner_x = area.x.saturating_add(1);
        let mut cur_y = area.y.saturating_add(1);
        let max_inner_y = area.bottom().saturating_sub(1);

        let red = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xF8, 0x71, 0x71)
        };
        let purple = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xC4, 0xB5, 0xFD)
        };
        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };
        let text_muted = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x64, 0x6A, 0x86)
        };
        let highlight_bg = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x23, 0x27, 0x38)
        };

        let selected_channel = all_channels.get(self.selected_idx).cloned();

        let mut items: Vec<ListItem> = Vec::new();
        let mut row_budget = max_rows;

        if snap.live_channels.is_empty() && snap.offline_channels.is_empty() {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                " Loading followed channels...",
                Style::default().fg(text_secondary),
            )])));
        } else {
            for stream in snap.live_channels.iter().skip(self.scroll_offset) {
                if row_budget < 2 {
                    break;
                }
                let is_selected = selected_channel
                    .as_ref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&stream.user_login));

                let viewers = stream.viewers_compact();
                let name_budget = inner_w.saturating_sub(viewers.len() + 9).max(6);
                let name = truncate_str(&stream.user_name, name_budget);
                let pad = inner_w
                    .saturating_sub(UnicodeWidthStr::width(name.as_str()) + viewers.len() + 7)
                    .max(1);

                let row_style = if is_selected {
                    Style::default().bg(highlight_bg)
                } else {
                    Style::default()
                };

                let line1 = Line::from(vec![
                    Span::styled(
                        if is_selected { "▸" } else { " " },
                        Style::default().fg(purple).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("● ", Style::default().fg(red).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        name,
                        Style::default()
                            .fg(if is_selected { purple } else { text_primary })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(viewers, Style::default().fg(red)),
                    Span::styled(" [×]", Style::default().fg(text_muted)),
                ]);

                let game = if stream.game_name.is_empty() {
                    "Just Chatting"
                } else {
                    stream.game_name.as_str()
                };
                let line2 = Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        truncate_str(game, inner_w.saturating_sub(3)),
                        Style::default().fg(text_secondary),
                    ),
                ]);

                if cur_y < max_inner_y {
                    let h = 2.min(max_inner_y.saturating_sub(cur_y));
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
                    self.sidebar_hitboxes.push((
                        Rect::new(inner_x, cur_y, full_w.saturating_sub(unf_w), h),
                        stream.user_login.clone(),
                    ));
                    cur_y = cur_y.saturating_add(h);
                }

                items.push(ListItem::new(vec![line1, line2]).style(row_style));
                row_budget = row_budget.saturating_sub(2);
            }

            let skip_offline = self.scroll_offset.saturating_sub(snap.live_channels.len());
            if row_budget >= 2 && !snap.offline_channels.is_empty() {
                if skip_offline == 0 {
                    items.push(ListItem::new(Line::from(vec![Span::styled(
                        format!(" ── OFFLINE ({}) ──", snap.offline_channels.len()),
                        Style::default().fg(text_muted),
                    )])));
                    cur_y = cur_y.saturating_add(1);
                    row_budget = row_budget.saturating_sub(1);
                }

                for off in snap.offline_channels.iter().skip(skip_offline) {
                    if row_budget == 0 {
                        break;
                    }
                    let is_selected = selected_channel
                        .as_ref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(off));
                    let row_style = if is_selected {
                        Style::default().bg(highlight_bg)
                    } else {
                        Style::default()
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
                            off.clone(),
                        ));
                        self.sidebar_hitboxes.push((
                            Rect::new(inner_x, cur_y, full_w.saturating_sub(unf_w), 1),
                            off.clone(),
                        ));
                        cur_y = cur_y.saturating_add(1);
                    }

                    let name_trunc = truncate_str(off, inner_w.saturating_sub(7));
                    let pad_len = inner_w
                        .saturating_sub(UnicodeWidthStr::width(name_trunc.as_str()) + 6)
                        .max(1);
                    items.push(
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                if is_selected { "▸" } else { " " },
                                Style::default().fg(purple).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled("○ ", Style::default().fg(text_muted)),
                            Span::styled(
                                name_trunc,
                                Style::default().fg(if is_selected {
                                    text_primary
                                } else {
                                    text_secondary
                                }),
                            ),
                            Span::raw(" ".repeat(pad_len)),
                            Span::styled("[×]", Style::default().fg(text_muted)),
                        ]))
                        .style(row_style),
                    );
                    row_budget = row_budget.saturating_sub(1);
                }
            }
        }

        f.render_widget(List::new(items).block(block), area);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders the Twitch Browse directory (Following, Top Live, Categories, and Search) + quick-resume bar"
    )]
    fn draw_main_content(&mut self, f: &mut Frame, area: Rect, all_channels: &[String]) {
        self.grid_area = Some(area);
        self.card_hitboxes.clear();
        self.category_hitboxes.clear();
        self.chip_hitboxes.clear();
        self.search_bar_rect = None;

        let snap = self.sidebar.borrow().snapshot.clone();
        let selected_channel = all_channels.get(self.selected_idx).cloned();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(6)])
            .split(area);

        let live_area = chunks.first().copied().unwrap_or(area);
        let bottom_area = chunks.get(1).copied().unwrap_or_default();

        let is_loading_more = self.top_loading_more || self.category_loading || self.search_loading;
        let loading_suffix = if is_loading_more {
            " · loading more..."
        } else {
            ""
        };

        let (header_title, count_str, hint_str) = match self.browse_tab {
            BrowseTab::Following => {
                if snap.live_channels.is_empty() {
                    (
                        "NO FOLLOWED LIVE — SHOWING TOP LIVE STREAMS",
                        format!("{}{loading_suffix}", snap.top_streams.len()),
                        "Tab: Switch View · Scroll/j/k for more · Click Card to Watch",
                    )
                } else {
                    (
                        "FOLLOWED LIVE CHANNELS",
                        snap.live_channels.len().to_string(),
                        "Tab: Top Live / Categories · Click Card or 1-9 to Watch",
                    )
                }
            }
            BrowseTab::TopLive => (
                "BROWSE TOP LIVE STREAMS ON TWITCH",
                format!("{}{loading_suffix}", snap.top_streams.len()),
                "Tab: Categories · Scroll/j/k for more · Click Card or 1-9 to Watch",
            ),
            BrowseTab::Categories => {
                if let Some(cat) = &self.selected_category {
                    (
                        "BROWSE CATEGORY STREAMS",
                        format!(
                            "{} ({}{loading_suffix})",
                            cat.name,
                            self.category_streams.len()
                        ),
                        "Scroll/j/k for more · Click category chip or Backspace to clear",
                    )
                } else {
                    (
                        "BROWSE TOP CATEGORIES & GAMES",
                        snap.top_categories.len().to_string(),
                        "Click any category pill below to load live streams",
                    )
                }
            }
            BrowseTab::Search => (
                "SEARCH LIVE TWITCH CHANNELS",
                format!("{}{loading_suffix}", self.search_results.len()),
                "Press / or click search box · Scroll/j/k for more",
            ),
        };

        let live_header = [
            TitleStyle::Combined(header_title, &count_str),
            TitleStyle::Single(hint_str),
        ];
        let live_block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line(&live_header, *TITLE_STYLE)));

        let inner_live = live_block.inner(live_area);
        f.render_widget(live_block, live_area);

        let mut grid_rect = inner_live;

        if self.browse_tab == BrowseTab::Categories && inner_live.height >= 6 {
            let sub = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(4)])
                .split(inner_live);
            if let Some(&cat_bar) = sub.first() {
                self.draw_category_pills(f, cat_bar, &snap.top_categories);
            }
            if let Some(&rest) = sub.get(1) {
                grid_rect = rest;
            }
        } else if (self.browse_tab == BrowseTab::Search || self.search_input.is_focused())
            && inner_live.height >= 6
        {
            let sub = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(4)])
                .split(inner_live);
            if let Some(&s_bar) = sub.first() {
                self.search_bar_rect = Some(s_bar);
                if self.search_input.is_focused() {
                    let _ = self.search_input.draw(f, Some(s_bar));
                } else {
                    let label = if self.search_query.is_empty() {
                        " Press [/] or click here to search live Twitch streams & categories...".to_string()
                    } else {
                        format!(
                            " Search query: \"{}\" ({} live results) — Press [/] or click to edit",
                            self.search_query,
                            self.search_results.len()
                        )
                    };
                    let sb_block = Block::default()
                        .borders(Borders::ALL)
                        .border_type(self.config.frontend.border_type.clone().into())
                        .border_style(*BORDER_DARK_STYLE);
                    f.render_widget(Paragraph::new(Line::from(label)).block(sb_block), s_bar);
                }
            }
            if let Some(&rest) = sub.get(1) {
                grid_rect = rest;
            }
        }

        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };

        let streams = self.active_grid_streams();

        if streams.is_empty() {
            let status_line = if self.category_loading || self.search_loading {
                "  Fetching live streams from Twitch Helix API..."
            } else if self.browse_tab == BrowseTab::Search && self.search_query.is_empty() {
                "  Type a channel or topic (e.g., \"asmr\", \"music\", \"chess\") and press Enter to browse live streams."
            } else if self.browse_tab == BrowseTab::Search {
                "  No live channels matched your search query. Try another keyword!"
            } else {
                "  Loading live streams from Twitch API... (Press [r] to refresh or [Tab] to switch Browse tabs)"
            };
            let empty_msg = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    status_line,
                    Style::default().fg(text_secondary),
                )),
                Line::from(Span::styled(
                    "  You can also select any channel from the left sidebar, Favorites/Recent below, or press [s] / [f].",
                    Style::default().fg(text_secondary),
                )),
            ]);
            f.render_widget(empty_msg, grid_rect);
        } else {
            let cols = if grid_rect.width >= 108 {
                3usize
            } else if grid_rect.width >= 68 {
                2usize
            } else {
                1usize
            };
            let card_h = 6u16;
            let rows_visible = usize::from(grid_rect.height / card_h).max(1);
            self.last_grid_cols = cols;
            self.last_grid_rows = rows_visible;
            let total_rows = streams.len().div_ceil(cols);
            let max_start_row = total_rows.saturating_sub(rows_visible);
            if self.card_scroll_offset > max_start_row {
                self.card_scroll_offset = max_start_row;
            }
            let col_constraints: Vec<Constraint> = (0..cols)
                .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
                .collect();

            let now = Utc::now();
            let start_row = self.card_scroll_offset;

            for row_idx in 0..rows_visible {
                let y = grid_rect
                    .y
                    .saturating_add(u16::try_from(row_idx).unwrap_or(0).saturating_mul(card_h));
                if y.saturating_add(card_h) > grid_rect.bottom() {
                    break;
                }
                let row_rect = Rect::new(grid_rect.x, y, grid_rect.width, card_h);
                let col_rects = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints.clone())
                    .split(row_rect);

                for col_idx in 0..cols {
                    let stream_idx = (start_row + row_idx) * cols + col_idx;
                    let Some(stream) = streams.get(stream_idx) else {
                        break;
                    };
                    let Some(&card_rect) = col_rects.get(col_idx) else {
                        continue;
                    };

                    let is_selected = selected_channel
                        .as_ref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&stream.user_login));

                    self.card_hitboxes
                        .push((card_rect, stream.user_login.clone()));
                    self.draw_stream_card(
                        f,
                        card_rect,
                        stream,
                        stream_idx + 1,
                        is_selected,
                        now,
                    );
                }
            }
        }

        // Bottom quick-access bar: Favorites & Recent Channels
        self.draw_quick_channels_bar(f, bottom_area, selected_channel.as_deref());
    }

    fn draw_category_pills(
        &mut self,
        f: &mut Frame,
        area: Rect,
        categories: &[CategoryMetadata],
    ) {
        let cyan = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x38, 0xBD, 0xF8)
        };
        let purple = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xA9, 0x70, 0xFF)
        };
        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };

        let mut row1 = vec![Span::styled(
            " Categories: ",
            Style::default().fg(purple).add_modifier(Modifier::BOLD),
        )];
        let mut row2 = vec![Span::raw(" ")];

        let mut x1 = area.x.saturating_add(13);
        let mut x2 = area.x.saturating_add(1);
        let y1 = area.y;
        let y2 = area.y.saturating_add(1);

        for cat in categories {
            let is_active = self
                .selected_category
                .as_ref()
                .is_some_and(|c| c.id == cat.id);
            let pill = format!(" [{}] ", cat.name);
            let w = u16::try_from(UnicodeWidthStr::width(pill.as_str())).unwrap_or(8);
            let style = if is_active {
                Style::default()
                    .fg(cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(text_primary)
            };

            if x1.saturating_add(w + 1) < area.right() {
                self.category_hitboxes
                    .push((Rect::new(x1, y1, w, 1), cat.clone()));
                row1.push(Span::styled(pill, style));
                row1.push(Span::styled(" ", Style::default().fg(text_secondary)));
                x1 = x1.saturating_add(w + 1);
            } else if area.height >= 2 && x2.saturating_add(w + 1) < area.right() {
                self.category_hitboxes
                    .push((Rect::new(x2, y2, w, 1), cat.clone()));
                row2.push(Span::styled(pill, style));
                row2.push(Span::styled(" ", Style::default().fg(text_secondary)));
                x2 = x2.saturating_add(w + 1);
            }
        }

        let lines = if area.height >= 2 {
            vec![Line::from(row1), Line::from(row2)]
        } else {
            vec![Line::from(row1)]
        };
        f.render_widget(Paragraph::new(lines), area);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders a stream card with title, category, tags, uptime, and viewer count"
    )]
    fn draw_stream_card(
        &self,
        f: &mut Frame,
        rect: Rect,
        stream: &StreamMetadata,
        badge_num: usize,
        is_selected: bool,
        now: chrono::DateTime<Utc>,
    ) {
        let red = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xF8, 0x71, 0x71)
        };
        let purple = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xA9, 0x70, 0xFF)
        };
        let cyan = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x38, 0xBD, 0xF8)
        };
        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };
        let tag_fg = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xC4, 0xB5, 0xFD)
        };

        let border_style = if is_selected {
            Style::default().fg(purple).add_modifier(Modifier::BOLD)
        } else {
            *BORDER_DARK_STYLE
        };

        let uptime = stream.uptime_text(now);
        let right_meta = if uptime.is_empty() {
            format!(" ♥ {} ", stream.viewers_compact())
        } else {
            format!(" ♥ {} · {} ", stream.viewers_compact(), uptime)
        };

        let card_block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(border_style)
            .title(Line::from(vec![
                Span::styled(
                    format!(" [{badge_num}] "),
                    Style::default().fg(purple).add_modifier(Modifier::BOLD),
                ),
                Span::styled("● LIVE ", Style::default().fg(red).add_modifier(Modifier::BOLD)),
                Span::styled(
                    stream.user_name.clone(),
                    Style::default()
                        .fg(text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]))
            .title_bottom(
                Line::from(Span::styled(right_meta, Style::default().fg(red)))
                    .alignment(Alignment::Right),
            );

        let inner_w = usize::from(rect.width.saturating_sub(3)).max(10);
        let game = if stream.game_name.is_empty() {
            "Just Chatting"
        } else {
            stream.game_name.as_str()
        };

        let mut lines = vec![
            Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    truncate_str(game, inner_w),
                    Style::default().fg(cyan).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    truncate_str(&stream.title, inner_w),
                    Style::default().fg(text_primary),
                ),
            ]),
        ];

        if stream.tags.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(" twitch.tv/{}", stream.user_login),
                Style::default().fg(text_secondary),
            )));
        } else {
            let mut tag_spans = vec![Span::raw(" ")];
            let mut used = 1usize;
            for tag in stream.tags.iter().take(4) {
                let chip = format!("[{tag}] ");
                if used + chip.len() > inner_w {
                    break;
                }
                used += chip.len();
                tag_spans.push(Span::styled(chip, Style::default().fg(tag_fg)));
            }
            lines.push(Line::from(tag_spans));
        }

        let para = Paragraph::new(lines)
            .block(card_block)
            .wrap(Wrap { trim: false });
        f.render_widget(para, rect);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Renders the bottom quick-access bar for favorite and recently watched channels"
    )]
    fn draw_quick_channels_bar(
        &mut self,
        f: &mut Frame,
        area: Rect,
        selected_channel: Option<&str>,
    ) {
        if area.height < 3 {
            return;
        }

        let purple = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xA9, 0x70, 0xFF)
        };
        let green = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x50, 0xC8, 0x78)
        };
        let text_primary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0xEE, 0xF2, 0xFF)
        };
        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };

        let header = [
            TitleStyle::Single("RECENT & FAVORITE CHANNELS"),
            TitleStyle::Single("Click any chip to open chat"),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line(&header, *TITLE_STYLE)));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let snap = &self.sidebar.borrow().snapshot;
        let favorites = self.config.frontend.favorite_channels.clone();
        let recents = self
            .storage
            .borrow()
            .get_last_n("channels", 8, true)
            .unwrap_or_default();

        let mut combined: Vec<(String, bool)> = Vec::new();
        for ch in &favorites {
            let clean = ch.trim().trim_start_matches('#').to_lowercase();
            if !clean.is_empty() && !combined.iter().any(|(c, _)| c == &clean) {
                combined.push((clean, true));
            }
        }
        for ch in &recents {
            let clean = ch.trim().trim_start_matches('#').to_lowercase();
            if !clean.is_empty() && !combined.iter().any(|(c, _)| c == &clean) {
                combined.push((clean, false));
            }
        }

        let mut row1_spans = vec![Span::raw(" ")];
        let mut row2_spans = vec![Span::raw(" ")];
        let mut x1 = inner.x.saturating_add(1);
        let mut x2 = inner.x.saturating_add(1);
        let y1 = inner.y;
        let y2 = inner.y.saturating_add(2.min(inner.height.saturating_sub(1)));

        for (ch, is_fav) in combined {
            let is_live = snap
                .live_channels
                .iter()
                .any(|s| s.user_login.eq_ignore_ascii_case(&ch));
            let is_sel = selected_channel.is_some_and(|s| s.eq_ignore_ascii_case(&ch));

            let prefix = if is_fav { "★ " } else { "" };
            let dot = if is_live { "● " } else { "○ " };
            let label = format!(" {prefix}{dot}{ch} ");
            let w = u16::try_from(UnicodeWidthStr::width(label.as_str())).unwrap_or(8);

            let chip_style = if is_sel {
                Style::default()
                    .fg(purple)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else if is_live {
                Style::default().fg(green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(text_primary)
            };

            if x1.saturating_add(w + 2) < inner.right() {
                self.chip_hitboxes.push((Rect::new(x1, y1, w, 1), ch));
                row1_spans.push(Span::styled(label, chip_style));
                row1_spans.push(Span::styled("  ", Style::default().fg(text_secondary)));
                x1 = x1.saturating_add(w + 2);
            } else if inner.height >= 3 && x2.saturating_add(w + 2) < inner.right() {
                self.chip_hitboxes.push((Rect::new(x2, y2, w, 1), ch));
                row2_spans.push(Span::styled(label, chip_style));
                row2_spans.push(Span::styled("  ", Style::default().fg(text_secondary)));
                x2 = x2.saturating_add(w + 2);
            }
        }

        let lines = if inner.height >= 3 {
            vec![
                Line::from(row1_spans),
                Line::from(""),
                Line::from(row2_spans),
            ]
        } else {
            vec![Line::from(row1_spans)]
        };

        f.render_widget(Paragraph::new(lines), inner);
    }

    #[must_use]
    pub fn keybind_badges(&self) -> Vec<(Rect, String)> {
        let mut out = Vec::new();
        for (i, (rect, tab)) in self.tab_rects.iter().enumerate() {
            let label = match tab {
                BrowseTab::Search => "/: Search".to_string(),
                _ if i == 0 => "Tab: Views".to_string(),
                _ => "Tab".to_string(),
            };
            out.push((*rect, label));
        }

        let keybinds = &self.config.keybinds.dashboard;
        let action_keys = [
            format!("{} : Follow", get_keybind_text(&keybinds.followed_channels_search)),
            format!("{} : Chan", get_keybind_text(&keybinds.recent_channels_search)),
            "r : Refresh".to_string(),
            format!("{} : Help", get_keybind_text(&keybinds.help)),
            format!("{} : Quit", get_keybind_text(&keybinds.quit)),
        ];
        for (i, rect) in self.action_rects.iter().enumerate() {
            if let Some(lbl) = action_keys.get(i) {
                out.push((*rect, lbl.clone()));
            }
        }

        if let Some(sb_rect) = self.search_bar_rect {
            out.push((
                Rect::new(sb_rect.x.saturating_add(2), sb_rect.y, 16, 1),
                "/ : Search Input".to_string(),
            ));
        }

        if let Some((cat_rect, _)) = self.category_hitboxes.first() {
            out.push((*cat_rect, "←/→ : Category".to_string()));
        }

        for (i, (card_rect, _)) in self.card_hitboxes.iter().enumerate() {
            let badge = if i < 9 {
                format!("{} / Enter", i + 1)
            } else {
                "j/k + Enter".to_string()
            };
            out.push((
                Rect::new(
                    card_rect.x.saturating_add(1),
                    card_rect.y,
                    14.min(card_rect.width),
                    1,
                ),
                badge,
            ));
        }

        if let Some((sb_item_rect, _)) = self.sidebar_hitboxes.first() {
            out.push((
                Rect::new(sb_item_rect.x, sb_item_rect.y, 14.min(sb_item_rect.width), 1),
                "j/k + Enter".to_string(),
            ));
        }

        if let Some((unf_rect, _)) = self.unfollow_hitboxes.first() {
            out.push((
                Rect::new(unf_rect.x.saturating_sub(8), unf_rect.y, 12, 1),
                "u/Del : Unf".to_string(),
            ));
        }

        if let Some((chip_rect, _)) = self.chip_hitboxes.first() {
            out.push((*chip_rect, "s : Switch".to_string()));
        }

        out
    }

    #[must_use]
    pub const fn is_any_input_focused(&self) -> bool {
        self.channel_input.is_focused()
            || self.following.is_focused()
            || self.search_input.is_focused()
    }
}

fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max_width {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

impl Component for DashboardWidget {
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        let r = area.unwrap_or_else(|| f.area());
        f.render_widget(Clear, r);

        let all_channels = self.all_selectable_channels();
        if self.selected_idx >= all_channels.len() && !all_channels.is_empty() {
            self.selected_idx = all_channels.len() - 1;
        }

        let nav_height = if r.height >= 20 {
            7
        } else if r.height >= 14 {
            5
        } else {
            3
        };
        let v_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(nav_height), Constraint::Min(1)])
            .split(r);

        if let Some(&nav_rect) = v_chunks.first() {
            self.draw_top_navbar(f, nav_rect);
        }

        if let Some(&body_rect) = v_chunks.get(1) {
            if body_rect.width >= 76 {
                let h_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(34), Constraint::Min(40)])
                    .split(body_rect);
                if let Some(&left_rect) = h_chunks.first() {
                    self.draw_left_sidebar(f, left_rect, &all_channels);
                }
                if let Some(&right_rect) = h_chunks.get(1) {
                    self.draw_main_content(f, right_rect, &all_channels);
                }
            } else {
                self.draw_main_content(f, body_rect, &all_channels);
            }
        }

        if self.channel_input.is_focused() {
            self.channel_input.draw(f, None)?;
        } else if self.following.is_focused() {
            self.following.draw(f, None)?;
        }

        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Handles mouse clicks, scroll wheel, live search bar, category filters, and keyboard navigation across the Twitch home screen"
    )]
    async fn event(&mut self, event: &Event) -> Result<()> {
        if self.channel_input.is_focused() {
            return self.channel_input.event(event).await;
        } else if self.following.is_focused() {
            return self.following.event(event).await;
        }

        if self.search_input.is_focused() {
            if matches!(event, Event::Input(Key::Esc)) {
                self.search_input.toggle_focus();
                return Ok(());
            }
            if matches!(event, Event::Input(Key::Enter)) {
                let q = self.search_input.to_string();
                self.search_input.toggle_focus();
                self.trigger_live_search(&q);
                return Ok(());
            }
            if let Event::Mouse(mouse) = event {
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && !self.search_input.contains(mouse.column, mouse.row)
                {
                    self.search_input.toggle_focus();
                } else {
                    return self.search_input.event(event).await;
                }
            } else {
                return self.search_input.event(event).await;
            }
        }

        let all_channels = self.all_selectable_channels();

        if let Event::Mouse(mouse) = event {
            let pos = Position::new(mouse.column, mouse.row);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if self.sidebar_area.is_some_and(|r| r.contains(pos)) {
                        self.scroll_offset = self.scroll_offset.saturating_sub(2);
                    } else if self.grid_area.is_some_and(|r| r.contains(pos)) {
                        self.card_scroll_offset = self.card_scroll_offset.saturating_sub(1);
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollDown => {
                    if self.sidebar_area.is_some_and(|r| r.contains(pos)) {
                        self.scroll_offset = self.scroll_offset.saturating_add(2);
                    } else if self.grid_area.is_some_and(|r| r.contains(pos)) {
                        self.scroll_grid_down();
                    }
                    return Ok(());
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    for (rect, tab) in &self.tab_rects {
                        if rect.contains(pos) {
                            let next_tab = tab.clone();
                            self.browse_tab = next_tab.clone();
                            self.card_scroll_offset = 0;
                            self.selected_idx = 0;
                            if next_tab == BrowseTab::Search && !self.search_input.is_focused() {
                                self.search_input.toggle_focus();
                            } else if next_tab == BrowseTab::Categories
                                && self.selected_category.is_none()
                            {
                                let first_cat = self
                                    .sidebar
                                    .borrow()
                                    .snapshot
                                    .top_categories
                                    .first()
                                    .cloned();
                                if let Some(cat) = first_cat {
                                    self.select_category(cat);
                                }
                            }
                            return Ok(());
                        }
                    }

                    if self.search_bar_rect.is_some_and(|r| r.contains(pos)) {
                        if !self.search_input.is_focused() {
                            self.search_input.toggle_focus();
                        }
                        return Ok(());
                    }

                    for (rect, cat) in &self.category_hitboxes {
                        if rect.contains(pos) {
                            let chosen = cat.clone();
                            self.select_category(chosen);
                            return Ok(());
                        }
                    }

                    for (rect, channel) in &self.unfollow_hitboxes {
                        if rect.contains(pos) {
                            let target = channel.clone();
                            self.toggle_follow(&target);
                            return Ok(());
                        }
                    }

                    for (rect, channel) in self
                        .card_hitboxes
                        .iter()
                        .chain(self.sidebar_hitboxes.iter())
                        .chain(self.chip_hitboxes.iter())
                    {
                        if rect.contains(pos) {
                            let target = channel.clone();
                            self.join_channel(&target).await?;
                            return Ok(());
                        }
                    }

                    if let Some((idx, _)) = self
                        .action_rects
                        .iter()
                        .enumerate()
                        .find(|(_, r)| r.contains(pos))
                    {
                        match idx {
                            0 => self.following.toggle_focus().await,
                            1 => self.channel_input.toggle_focus(),
                            2 => {
                                let last = self
                                    .storage
                                    .borrow()
                                    .last_channel()
                                    .unwrap_or_else(|| self.config.twitch.channel.clone());
                                let mut tracked = self.config.frontend.favorite_channels.clone();
                                tracked.extend(self.storage.borrow().followed_channels());
                                let unfollowed = self.storage.borrow().unfollowed_channels();
                                self.sidebar.borrow_mut().maybe_refresh(
                                    &last,
                                    tracked,
                                    unfollowed,
                                    &self.twitch_oauth,
                                    true,
                                );
                            }
                            3 => {
                                self.event_tx
                                    .send(Event::Internal(InternalEvent::SwitchState(State::Help)))
                                    .await?;
                            }
                            4 => {
                                self.event_tx
                                    .send(Event::Internal(InternalEvent::Quit))
                                    .await?;
                            }
                            _ => {}
                        }
                        return Ok(());
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        if let Event::Input(key) = event {
            let keybinds = &self.config.keybinds.dashboard;
            match key {
                key if keybinds.quit.contains(key) => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::Quit))
                        .await?;
                }
                key if keybinds.recent_channels_search.contains(key) => {
                    self.channel_input.toggle_focus();
                }
                key if keybinds.followed_channels_search.contains(key) => {
                    self.following.toggle_focus().await;
                }
                key if keybinds.help.contains(key) => {
                    self.event_tx
                        .send(Event::Internal(InternalEvent::SwitchState(State::Help)))
                        .await?;
                }
                Key::Char('/') => {
                    self.browse_tab = BrowseTab::Search;
                    if !self.search_input.is_focused() {
                        self.search_input.toggle_focus();
                    }
                }
                Key::Char('u') | Key::Delete => {
                    if let Some(ch) = all_channels.get(self.selected_idx).cloned() {
                        self.toggle_follow(&ch);
                    }
                }
                Key::Char('r') => {
                    let last = self
                        .storage
                        .borrow()
                        .last_channel()
                        .unwrap_or_else(|| self.config.twitch.channel.clone());
                    let mut tracked = self.config.frontend.favorite_channels.clone();
                    tracked.extend(self.storage.borrow().followed_channels());
                    let unfollowed = self.storage.borrow().unfollowed_channels();
                    self.sidebar.borrow_mut().maybe_refresh(
                        &last,
                        tracked,
                        unfollowed,
                        &self.twitch_oauth,
                        true,
                    );
                    if let Some(cat) = self.selected_category.clone() {
                        self.select_category(cat);
                    }
                    if self.browse_tab == BrowseTab::Search && !self.search_query.is_empty() {
                        let q = self.search_query.clone();
                        self.trigger_live_search(&q);
                    }
                }
                Key::Tab => {
                    self.browse_tab = match self.browse_tab {
                        BrowseTab::Following => BrowseTab::TopLive,
                        BrowseTab::TopLive => BrowseTab::Categories,
                        BrowseTab::Categories => BrowseTab::Search,
                        BrowseTab::Search => BrowseTab::Following,
                    };
                    self.card_scroll_offset = 0;
                    self.selected_idx = 0;
                    if self.browse_tab == BrowseTab::Categories && self.selected_category.is_none()
                    {
                        let first_cat = self
                            .sidebar
                            .borrow()
                            .snapshot
                            .top_categories
                            .first()
                            .cloned();
                        if let Some(cat) = first_cat {
                            self.select_category(cat);
                        }
                    }
                }
                Key::Backspace => {
                    if self.browse_tab == BrowseTab::Categories {
                        self.selected_category = None;
                        self.category_streams.clear();
                    }
                }
                Key::Down | Key::Char('j') => {
                    let grid_len = self.active_grid_streams().len();
                    let cols = self.last_grid_cols.max(1);
                    if grid_len > 0 && self.selected_idx < grid_len {
                        let next_in_col = self.selected_idx + cols;
                        if next_in_col < grid_len {
                            self.selected_idx = next_in_col;
                        } else if self.selected_idx + 1 < grid_len {
                            self.selected_idx += 1;
                        } else {
                            self.maybe_load_more();
                        }
                        self.sync_card_scroll_to_selection();
                    } else if !all_channels.is_empty() {
                        self.selected_idx = (self.selected_idx + 1) % all_channels.len();
                    }
                }
                Key::Up | Key::Char('k') => {
                    let grid_len = self.active_grid_streams().len();
                    let cols = self.last_grid_cols.max(1);
                    if grid_len > 0 && self.selected_idx < grid_len {
                        self.selected_idx = self.selected_idx.saturating_sub(cols);
                        self.sync_card_scroll_to_selection();
                    } else if !all_channels.is_empty() {
                        self.selected_idx = if self.selected_idx == 0 {
                            all_channels.len() - 1
                        } else {
                            self.selected_idx - 1
                        };
                    }
                }
                Key::ScrollDown | Key::Ctrl('d') => {
                    let grid_len = self.active_grid_streams().len();
                    let step = self.last_grid_cols.max(1) * self.last_grid_rows.max(1);
                    if grid_len > 0 {
                        self.selected_idx = (self.selected_idx + step).min(grid_len - 1);
                        self.sync_card_scroll_to_selection();
                        self.maybe_load_more();
                    }
                }
                Key::ScrollUp | Key::Ctrl('u') => {
                    let grid_len = self.active_grid_streams().len();
                    let step = self.last_grid_cols.max(1) * self.last_grid_rows.max(1);
                    if grid_len > 0 {
                        self.selected_idx = self.selected_idx.saturating_sub(step);
                        self.sync_card_scroll_to_selection();
                    }
                }
                Key::Home => {
                    self.selected_idx = 0;
                    self.card_scroll_offset = 0;
                }
                Key::End => {
                    let grid_len = self.active_grid_streams().len();
                    if grid_len > 0 {
                        self.selected_idx = grid_len - 1;
                        self.sync_card_scroll_to_selection();
                        self.maybe_load_more();
                    }
                }
                Key::Right | Key::Char('l') => {
                    if self.browse_tab == BrowseTab::Categories {
                        let cats = self.sidebar.borrow().snapshot.top_categories.clone();
                        if !cats.is_empty() {
                            let cur_pos = self
                                .selected_category
                                .as_ref()
                                .and_then(|cur| cats.iter().position(|c| c.id == cur.id))
                                .map_or(0, |idx| (idx + 1) % cats.len());
                            if let Some(next_cat) = cats.get(cur_pos).cloned() {
                                self.select_category(next_cat);
                            }
                        }
                    } else if !all_channels.is_empty() {
                        let grid_len = self.active_grid_streams().len();
                        if grid_len > 0 && self.selected_idx + 1 >= grid_len {
                            self.maybe_load_more();
                        }
                        self.selected_idx = (self.selected_idx + 1) % all_channels.len();
                        self.sync_card_scroll_to_selection();
                    }
                }
                Key::Left | Key::Char('h') => {
                    if self.browse_tab == BrowseTab::Categories {
                        let cats = self.sidebar.borrow().snapshot.top_categories.clone();
                        if !cats.is_empty() {
                            let cur_pos = self
                                .selected_category
                                .as_ref()
                                .and_then(|cur| cats.iter().position(|c| c.id == cur.id))
                                .map_or(0, |idx| if idx == 0 { cats.len() - 1 } else { idx - 1 });
                            if let Some(prev_cat) = cats.get(cur_pos).cloned() {
                                self.select_category(prev_cat);
                            }
                        }
                    } else if !all_channels.is_empty() {
                        self.selected_idx = if self.selected_idx == 0 {
                            all_channels.len() - 1
                        } else {
                            self.selected_idx - 1
                        };
                        self.sync_card_scroll_to_selection();
                    }
                }
                key if keybinds.join.contains(key) => {
                    if let Some(ch) = all_channels.get(self.selected_idx).cloned() {
                        self.join_channel(&ch).await?;
                    } else {
                        let fallback = self
                            .storage
                            .borrow()
                            .last_channel()
                            .unwrap_or_else(|| self.config.twitch.channel.clone());
                        self.join_channel(&fallback).await?;
                    }
                }
                Key::Char(c) => {
                    if let Some(digit) = c.to_digit(10) {
                        if digit >= 1 {
                            let idx = (digit as usize) - 1;
                            if let Some(ch) = all_channels.get(idx).cloned() {
                                self.join_channel(&ch).await?;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}
