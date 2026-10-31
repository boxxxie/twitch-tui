use chrono::Utc;
use color_eyre::Result;
use tokio::sync::mpsc::Sender;
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
        sidebar::{SharedSidebar, StreamMetadata},
        state::State,
        storage::SharedStorage,
    },
    twitch::oauth::TwitchOauth,
    ui::components::{ChannelSwitcherWidget, Component, FollowingWidget},
    utils::{
        styles::{BORDER_DARK_STYLE, NO_COLOR, TEXT_DARK_STYLE, TITLE_STYLE},
        text::{TitleStyle, title_line},
    },
};

pub struct DashboardWidget {
    config: SharedCoreConfig,
    event_tx: Sender<Event>,
    storage: SharedStorage,
    sidebar: SharedSidebar,
    channel_input: ChannelSwitcherWidget,
    following: FollowingWidget,
    selected_idx: usize,
    scroll_offset: usize,
    card_scroll_offset: usize,
    sidebar_hitboxes: Vec<(Rect, String)>,
    card_hitboxes: Vec<(Rect, String)>,
    chip_hitboxes: Vec<(Rect, String)>,
    action_rects: Vec<Rect>,
    sidebar_area: Option<Rect>,
    grid_area: Option<Rect>,
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
        let following = FollowingWidget::new(config.clone(), twitch_oauth, event_tx.clone());

        Self {
            config,
            event_tx,
            storage,
            sidebar,
            channel_input,
            following,
            selected_idx: 0,
            scroll_offset: 0,
            card_scroll_offset: 0,
            sidebar_hitboxes: Vec::new(),
            card_hitboxes: Vec::new(),
            chip_hitboxes: Vec::new(),
            action_rects: Vec::new(),
            sidebar_area: None,
            grid_area: None,
        }
    }

    fn all_selectable_channels(&self) -> Vec<String> {
        let snap = &self.sidebar.borrow().snapshot;
        let mut out: Vec<String> = snap
            .live_channels
            .iter()
            .map(|s| s.user_login.clone())
            .collect();

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
        reason = "Renders the large AGY-style 5-row truecolor Twitch Glitch + TWT block-art logo alongside the navigation and action bar"
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
            (get_keybind_text(&keybinds.recent_channels_search), "Search"),
            (get_keybind_text(&keybinds.help), "Help"),
            (get_keybind_text(&keybinds.quit), "Quit"),
        ];

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

        // Row 1: Large Twitch Glitch mascot top + TWT block-art top + Browse tab + Action pills
        let mut row1_spans = vec![
            Span::raw("  "),
            Span::styled("▄████████████▄", p_main),
            Span::raw("   "),
            Span::styled("██████  ██    ██  ██████", p_bright),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled(
                "Browse",
                Style::default()
                    .fg(text_primary)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::raw("   "),
        ];

        let mut x = area.x.saturating_add(
            u16::try_from(
                row1_spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum::<usize>(),
            )
            .unwrap_or(56)
            .saturating_add(1),
        );
        let row1_y = area.y.saturating_add(1);

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
                    "○ OFFLINE — Select any live stream below",
                    Style::default().fg(amber),
                )
            },
        ];

        // Row 4: Glitch mascot lower jaw + TWT block-art row 4 + Live followed channel count
        let live_count = self.sidebar.borrow().snapshot.live_channels.len();
        let row4_spans = vec![
            Span::raw("  "),
            Span::styled("████████████▀▀", p_main),
            Span::styled("█", p_shadow),
            Span::raw("  "),
            Span::styled("  ██    ██▄██▄██    ██  ", p_main),
            Span::raw("   "),
            Span::styled("│ ", Style::default().fg(text_secondary)),
            Span::styled(
                format!("● {live_count} followed channels live"),
                Style::default().fg(if live_count > 0 {
                    green
                } else {
                    text_secondary
                }),
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
            Span::styled("Click any stream card or press ", Style::default().fg(text_secondary)),
            Span::styled("1-9", p_bright),
            Span::styled(" / ", Style::default().fg(text_secondary)),
            Span::styled("Enter", p_bright),
            Span::styled(" to watch", Style::default().fg(text_secondary)),
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

        let snap = self.sidebar.borrow().snapshot.clone();
        let live_count = snap.live_channels.len();
        let total_followed = live_count + snap.offline_channels.len();

        let title_str = format!("{live_count} live / {total_followed}");
        let header_spans = [
            TitleStyle::Combined("FOLLOWED CHANNELS", &title_str),
            TitleStyle::Single("Click or Enter"),
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
                let name_budget = inner_w.saturating_sub(viewers.len() + 5).max(6);
                let name = truncate_str(&stream.user_name, name_budget);
                let pad = inner_w
                    .saturating_sub(UnicodeWidthStr::width(name.as_str()) + viewers.len() + 3)
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
                    self.sidebar_hitboxes.push((
                        Rect::new(inner_x, cur_y, area.width.saturating_sub(2), h),
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
                        self.sidebar_hitboxes.push((
                            Rect::new(inner_x, cur_y, area.width.saturating_sub(2), 1),
                            off.clone(),
                        ));
                        cur_y = cur_y.saturating_add(1);
                    }

                    items.push(
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                if is_selected { "▸" } else { " " },
                                Style::default().fg(purple).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled("○ ", Style::default().fg(text_muted)),
                            Span::styled(
                                truncate_str(off, inner_w.saturating_sub(3)),
                                Style::default().fg(if is_selected {
                                    text_primary
                                } else {
                                    text_secondary
                                }),
                            ),
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
        reason = "Renders the Twitch homepage live stream card grid + quick-resume chips"
    )]
    fn draw_main_content(&mut self, f: &mut Frame, area: Rect, all_channels: &[String]) {
        self.grid_area = Some(area);
        self.card_hitboxes.clear();
        self.chip_hitboxes.clear();

        let snap = self.sidebar.borrow().snapshot.clone();
        let selected_channel = all_channels.get(self.selected_idx).cloned();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(6)])
            .split(area);

        let live_area = chunks.first().copied().unwrap_or(area);
        let bottom_area = chunks.get(1).copied().unwrap_or_default();

        let live_header = [
            TitleStyle::Combined("LIVE CHANNELS WE THINK YOU'LL LIKE", &snap.live_channels.len().to_string()),
            TitleStyle::Single("↑/↓/←/→ or Click Card to Watch"),
        ];
        let live_block = Block::default()
            .borders(Borders::ALL)
            .border_type(self.config.frontend.border_type.clone().into())
            .border_style(*BORDER_DARK_STYLE)
            .title(Line::from(title_line(&live_header, *TITLE_STYLE)));

        let inner_live = live_block.inner(live_area);
        f.render_widget(live_block, live_area);

        let text_secondary = if *NO_COLOR {
            Color::Reset
        } else {
            Color::Rgb(0x94, 0x9C, 0xBB)
        };

        if snap.live_channels.is_empty() {
            let empty_msg = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  No followed channels are currently live (or still fetching from Twitch API...).",
                    Style::default().fg(text_secondary),
                )),
                Line::from(Span::styled(
                    "  Select any channel from the left sidebar, Favorites/Recent below, or press [s] / [f] to search.",
                    Style::default().fg(text_secondary),
                )),
            ]);
            f.render_widget(empty_msg, inner_live);
        } else {
            let cols = if inner_live.width >= 108 {
                3usize
            } else if inner_live.width >= 68 {
                2usize
            } else {
                1usize
            };
            let card_h = 6u16;
            let rows_visible = usize::from(inner_live.height / card_h).max(1);
            let col_constraints: Vec<Constraint> = (0..cols)
                .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
                .collect();

            let now = Utc::now();
            let start_row = self.card_scroll_offset;

            for row_idx in 0..rows_visible {
                let y = inner_live
                    .y
                    .saturating_add(u16::try_from(row_idx).unwrap_or(0).saturating_mul(card_h));
                if y.saturating_add(card_h) > inner_live.bottom() {
                    break;
                }
                let row_rect = Rect::new(inner_live.x, y, inner_live.width, card_h);
                let col_rects = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(col_constraints.clone())
                    .split(row_rect);

                for col_idx in 0..cols {
                    let stream_idx = (start_row + row_idx) * cols + col_idx;
                    let Some(stream) = snap.live_channels.get(stream_idx) else {
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
            .title_bottom(Line::from(Span::styled(right_meta, Style::default().fg(red))).alignment(Alignment::Right));

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

        if !stream.tags.is_empty() {
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
        } else {
            lines.push(Line::from(Span::styled(
                format!(" twitch.tv/{}", stream.user_login),
                Style::default().fg(text_secondary),
            )));
        }

        let para = Paragraph::new(lines)
            .block(card_block)
            .wrap(Wrap { trim: false });
        f.render_widget(para, rect);
    }

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
        reason = "Handles mouse clicks, scroll wheel, and keyboard navigation across the Twitch home screen"
    )]
    async fn event(&mut self, event: &Event) -> Result<()> {
        if self.channel_input.is_focused() {
            return self.channel_input.event(event).await;
        } else if self.following.is_focused() {
            return self.following.event(event).await;
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
                        self.card_scroll_offset = self.card_scroll_offset.saturating_add(1);
                    }
                    return Ok(());
                }
                MouseEventKind::Down(MouseButton::Left) => {
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
                                self.event_tx
                                    .send(Event::Internal(InternalEvent::SwitchState(State::Help)))
                                    .await?;
                            }
                            3 => {
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
                Key::Down | Key::Char('j') | Key::Tab => {
                    if !all_channels.is_empty() {
                        self.selected_idx = (self.selected_idx + 1) % all_channels.len();
                    }
                }
                Key::Up | Key::Char('k') => {
                    if !all_channels.is_empty() {
                        self.selected_idx = if self.selected_idx == 0 {
                            all_channels.len() - 1
                        } else {
                            self.selected_idx - 1
                        };
                    }
                }
                Key::Right | Key::Char('l') => {
                    if !all_channels.is_empty() {
                        self.selected_idx = (self.selected_idx + 1) % all_channels.len();
                    }
                }
                Key::Left | Key::Char('h') => {
                    if !all_channels.is_empty() {
                        self.selected_idx = if self.selected_idx == 0 {
                            all_channels.len() - 1
                        } else {
                            self.selected_idx - 1
                        };
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
