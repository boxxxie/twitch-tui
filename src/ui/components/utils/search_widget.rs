use std::{clone::Clone, convert::From, iter::Iterator, vec::Vec};

use color_eyre::{Result, eyre::ContextCompat};
use tokio::sync::mpsc::Sender;
use tui::{
    Frame,
    crossterm::event::{MouseButton, MouseEventKind},
    layout::{Position, Rect},
    prelude::{Alignment, Margin},
    style::{Color, Modifier, Style},
    symbols::scrollbar,
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Scrollbar, ScrollbarOrientation,
        ScrollbarState, TitlePosition,
    },
};

use super::{InputWidget, popup_area};
use crate::{
    config::SharedCoreConfig,
    events::{Event, Key, TwitchAction, TwitchEvent},
    ui::components::{Component, ErrorWidget},
    utils::{
        sanitization::clean_channel_name,
        search::fuzzy_pattern_match,
        styles::{NO_COLOR, SEARCH_STYLE, TITLE_STYLE},
        text::{TitleStyle, title_line},
    },
};

pub trait SearchItemGetter<T>
where
    T: ToString,
{
    async fn get_items(&mut self) -> Result<Vec<T>>;

    fn remove_item(&mut self, _item: &str) {}
}

pub struct SearchWidget<T, U>
where
    T: ToString + Clone,
    U: SearchItemGetter<T>,
{
    config: SharedCoreConfig,
    event_tx: Sender<Event>,
    focused: bool,

    item_getter: U,
    items: Result<Vec<T>>,
    filtered_items: Option<Vec<T>>,

    list_state: ListState,
    search_input: InputWidget<()>,
    vertical_scroll_state: ScrollbarState,
    vertical_scroll: usize,
    last_list_rect: Option<Rect>,
    last_input_rect: Option<Rect>,

    error_widget: ErrorWidget,
}

impl<T, U> SearchWidget<T, U>
where
    T: ToString + Clone,
    U: SearchItemGetter<T>,
{
    pub fn new(
        config: SharedCoreConfig,
        event_tx: Sender<Event>,
        item_getter: U,
        title: &str,
        error_message: Vec<&'static str>,
    ) -> Self {
        let search_input = InputWidget::builder()
            .config(config.clone())
            .event_tx(event_tx.clone())
            .title(title)
            .build();

        // TODO: Only one error widget should exist throughout the entire application
        let error_widget = ErrorWidget::new(config.clone(), event_tx.clone(), error_message);

        Self {
            config,
            event_tx,
            focused: false,
            item_getter,
            items: Ok(Vec::default()),
            filtered_items: None,
            list_state: ListState::default(),
            search_input,
            vertical_scroll_state: ScrollbarState::default(),
            vertical_scroll: 0,
            last_list_rect: None,
            last_input_rect: None,
            error_widget,
        }
    }

    fn next(&mut self) -> Result<()> {
        let i = match self.list_state.selected() {
            Some(i) => {
                if let Some(filtered) = &self.filtered_items {
                    if i >= filtered.len().saturating_sub(1) {
                        filtered.len().saturating_sub(1)
                    } else {
                        i.saturating_add(1)
                    }
                } else {
                    let max = self
                        .items
                        .as_ref()
                        .ok()
                        .context("Failed to get items")?
                        .len()
                        .saturating_sub(1);

                    if i >= max { max } else { i.saturating_add(1) }
                }
            }
            None => 0,
        };

        self.list_state.select(Some(i));

        self.vertical_scroll = self.vertical_scroll.saturating_add(1);
        self.vertical_scroll_state = self.vertical_scroll_state.position(self.vertical_scroll);

        Ok(())
    }

    fn previous(&mut self) {
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| if i == 0 { 0 } else { i.saturating_sub(1) });
        self.list_state.select(Some(i));

        self.vertical_scroll = self.vertical_scroll.saturating_sub(1);
        self.vertical_scroll_state = self.vertical_scroll_state.position(self.vertical_scroll);
    }

    const fn unselect(&mut self) {
        self.list_state.select(None);
    }

    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    pub async fn toggle_focus(&mut self) {
        if !self.focused {
            self.items = self.item_getter.get_items().await;
        }

        if self.items.is_err() {
            self.error_widget.toggle_focus();
        }

        self.focused = !self.focused;
    }

    async fn select(&mut self) -> Result<()> {
        if let Some(i) = self.list_state.selected() {
            let selected_item = if let Some(v) = self.filtered_items.clone() {
                if v.is_empty() {
                    return Ok(());
                }

                v.get(i).context("Failed to get selected item")?.to_string()
            } else {
                self.items
                    .as_ref()
                    .ok()
                    .context("Failed to get items")?
                    .get(i)
                    .context("Failed to get selected item")?
                    .to_string()
            }
            .to_lowercase();

            self.toggle_focus().await;

            self.unselect();

            let selected_channel_trimmed = clean_channel_name(&selected_item);

            self.event_tx
                .send(Event::Twitch(TwitchEvent::Action(
                    TwitchAction::JoinChannel(selected_channel_trimmed),
                )))
                .await?;
        }

        Ok(())
    }
}

impl<T, U> Component for SearchWidget<T, U>
where
    T: ToString + Clone,
    U: SearchItemGetter<T>,
{
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        let r = area.unwrap_or_else(|| popup_area(f.area(), 60, 60));
        self.last_list_rect = Some(r);

        if self.error_widget.is_focused() {
            self.last_input_rect = None;
            return self.error_widget.draw(f, Some(r));
        }

        let mut items = Vec::default();
        let current_items = &self
            .items
            .as_ref()
            .map_or_else(|_| Vec::default(), Clone::clone);
        let current_input = self.search_input.to_string();

        if current_input.is_empty() {
            for item in current_items {
                items.push(ListItem::new(item.to_string()));
            }

            self.filtered_items = None;
        } else {
            let item_filter =
                |choice: String| -> Vec<usize> { fuzzy_pattern_match(&current_input, &choice) };

            let mut matched = Vec::default();

            for item in current_items.clone() {
                let matched_indices = item_filter(item.to_string());

                if matched_indices.is_empty() {
                    continue;
                }

                let line = item
                    .to_string()
                    .chars()
                    .enumerate()
                    .map(|(i, c)| {
                        if matched_indices.contains(&i) {
                            Span::styled(c.to_string(), *SEARCH_STYLE)
                        } else {
                            Span::raw(c.to_string())
                        }
                    })
                    .collect::<Vec<Span>>();

                items.push(ListItem::new(vec![Line::from(line)]));
                matched.push(item);
            }

            self.filtered_items = Some(matched);
        }

        let title_binding = [TitleStyle::Single("Following")];

        let list = List::new(items.clone())
            .block(
                Block::default()
                    .title(title_line(&title_binding, *TITLE_STYLE))
                    .borders(Borders::ALL)
                    .border_type(self.config.frontend.border_type.clone().into()),
            )
            .highlight_style(if *NO_COLOR {
                Style::default()
            } else {
                Style::default()
                    .bg(Color::Rgb(0x22, 0x36, 0x4D))
                    .fg(Color::Rgb(0x38, 0xBD, 0xF8))
                    .add_modifier(Modifier::BOLD)
            });

        f.render_widget(Clear, r);
        f.render_stateful_widget(list, r, &mut self.list_state);

        self.vertical_scroll_state = self.vertical_scroll_state.content_length(items.len());

        f.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .symbols(scrollbar::VERTICAL)
                .begin_symbol(None)
                .end_symbol(None),
            r.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut self.vertical_scroll_state,
        );

        let title_binding = format!(
            "{} / {}",
            self.list_state.selected().map_or(1, |i| i + 1),
            self.filtered_items
                .as_ref()
                .map_or(current_items.len(), Vec::len)
        );

        let title = [TitleStyle::Single(&title_binding)];

        let bottom_block = Block::default()
            .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
            .border_type(self.config.frontend.border_type.clone().into())
            .title(title_line(&title, Style::default()))
            .title_position(TitlePosition::Bottom)
            .title_alignment(Alignment::Right);

        let rect = Rect::new(r.x, r.bottom() - 1, r.width, 1);

        f.render_widget(bottom_block, rect);

        let input_rect = Rect::new(rect.x, rect.bottom(), rect.width, 3);
        self.last_input_rect = Some(input_rect);

        self.search_input.draw(f, Some(input_rect))?;

        Ok(())
    }

    async fn event(&mut self, event: &Event) -> Result<()> {
        if self.error_widget.is_focused() {
            if matches!(event, Event::Input(Key::Esc))
                || matches!(
                    event,
                    Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(_))
                )
            {
                self.error_widget.toggle_focus();
                self.toggle_focus().await;
            }
            return Ok(());
        }

        if let Event::Mouse(mouse) = event {
            let pos = Position::new(mouse.column, mouse.row);
            match mouse.kind {
                MouseEventKind::ScrollDown => {
                    self.next()?;
                    return Ok(());
                }
                MouseEventKind::ScrollUp => {
                    self.previous();
                    return Ok(());
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(list_rect) = self.last_list_rect {
                        let inner = list_rect.inner(Margin {
                            vertical: 1,
                            horizontal: 1,
                        });
                        if inner.contains(pos) {
                            let clicked_idx = self.list_state.offset()
                                + usize::from(mouse.row.saturating_sub(inner.y));
                            let total = self.filtered_items.as_ref().map_or_else(
                                || self.items.as_ref().map_or(0, Vec::len),
                                Vec::len,
                            );
                            if clicked_idx < total {
                                self.list_state.select(Some(clicked_idx));
                                self.select().await?;
                            }
                            return Ok(());
                        }
                    }

                    if self.last_input_rect.is_some_and(|r| r.contains(pos)) {
                        self.search_input.event(event).await?;
                        return Ok(());
                    }

                    if self.last_list_rect.is_none_or(|r| !r.contains(pos)) {
                        self.unselect();
                        self.toggle_focus().await;
                        return Ok(());
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    self.unselect();
                    self.toggle_focus().await;
                    return Ok(());
                }
                _ => {}
            }
        }

        if let Event::Input(key) = event {
            let keybinds = &self.config.keybinds.selection;
            match key {
                key if keybinds.back_to_previous_window.contains(key) => {
                    if self.list_state.selected().is_some() {
                        self.unselect();
                    } else {
                        self.toggle_focus().await;
                    }
                }
                key if keybinds.next_item.contains(key) => self.next()?,
                key if keybinds.prev_item.contains(key) => self.previous(),
                key if keybinds.delete_item.contains(key)
                    || matches!(key, Key::Delete | Key::Char('u')) =>
                {
                    if let Some(index) = self.list_state.selected() {
                        let target_str = if let Some(filtered) = &self.filtered_items {
                            filtered.get(index).map(ToString::to_string)
                        } else if let Ok(all) = &self.items {
                            all.get(index).map(ToString::to_string)
                        } else {
                            None
                        };
                        if let Some(val) = target_str {
                            self.item_getter.remove_item(&val);
                            if let Ok(all) = &mut self.items {
                                all.retain(|x| x.to_string() != val);
                            }
                            if let Some(filtered) = &mut self.filtered_items {
                                filtered.retain(|x| x.to_string() != val);
                            }
                        }
                    }
                }
                key if keybinds.select.contains(key) => {
                    self.select().await?;
                }
                _ => {
                    self.search_input.event(event).await?;

                    // Assuming that the user inputted something that modified the input
                    if let Some(v) = &self.filtered_items {
                        if !v.is_empty() {
                            self.list_state.select(Some(0));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
