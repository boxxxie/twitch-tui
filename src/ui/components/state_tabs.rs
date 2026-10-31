use color_eyre::Result;
use tui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    symbols::DOT,
    text::Line,
    widgets::{Block, Tabs},
};

use crate::{
    config::SharedCoreConfig,
    handlers::state::State,
    ui::components::Component,
    utils::{styles::STATE_TABS_STYLE, text::capitalize_first_char},
};

const STATE_TABS: [State; 3] = [State::Dashboard, State::Normal, State::Help];

#[derive(Debug, Clone, Default)]
pub struct StateTabsWidget {
    selected_tab: usize,
    tab_rects: Vec<(Rect, State)>,
}

impl StateTabsWidget {
    pub fn new(config: &SharedCoreConfig) -> Self {
        let selected_tab = Self::get_state_index(&config.terminal.first_state);

        Self {
            selected_tab,
            tab_rects: Vec::new(),
        }
    }

    fn get_state_index(state: &State) -> usize {
        STATE_TABS
            .iter()
            .position(|s| s == state)
            .unwrap_or_default()
    }

    pub fn select(&mut self, state: &State) {
        self.selected_tab = Self::get_state_index(state);
    }

    #[must_use]
    pub fn clicked_state(&self, column: u16, row: u16) -> Option<State> {
        let pos = Position::new(column, row);
        for (rect, state) in &self.tab_rects {
            if rect.contains(pos) {
                return Some(state.clone());
            }
        }
        None
    }
}

impl Component for StateTabsWidget {
    fn draw(&mut self, f: &mut Frame, area: Option<Rect>) -> Result<()> {
        let r = area.unwrap_or_else(|| f.area());

        self.tab_rects.clear();
        let mut x = r.x;
        let mut tab_titles = Vec::with_capacity(STATE_TABS.len());
        for (i, t) in STATE_TABS.iter().enumerate() {
            let label = capitalize_first_char(&t.to_string());
            let w = u16::try_from(label.chars().count() + 2).unwrap_or(6);
            self.tab_rects.push((Rect::new(x, r.y, w, 1), t.clone()));
            x = x.saturating_add(w);
            if i + 1 < STATE_TABS.len() {
                x = x.saturating_add(1);
            }
            tab_titles.push(Line::from(label));
        }

        let tabs = Tabs::new(tab_titles)
            .block(Block::default())
            .style(*STATE_TABS_STYLE)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .remove_modifier(Modifier::DIM)
                    .add_modifier(Modifier::UNDERLINED),
            )
            .divider(DOT)
            .select(self.selected_tab);

        f.render_widget(tabs, r);

        Ok(())
    }
}
