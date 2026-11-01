use std::time::{Duration, Instant};

use color_eyre::Result;
use tokio::sync::mpsc::{Receiver, Sender};
use tui::crossterm::event::{
    self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};

use crate::events::{Event, key::Key};

pub struct Events {
    rx: Receiver<Event>,
}

impl Events {
    pub fn new(delay: u64, tx: Sender<Event>, rx: Receiver<Event>) -> Self {
        let tick_rate = Duration::from_millis(delay.max(250));
        let actor = EventsThread::new(tx, tick_rate);
        std::thread::Builder::new()
            .name("twt-events".to_string())
            .spawn(move || {
                let _ = actor.run();
            })
            .ok();

        Self { rx }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

pub struct EventsThread {
    tx: Sender<Event>,
    tick_rate: Duration,
}

impl EventsThread {
    pub const fn new(tx: Sender<Event>, tick_rate: Duration) -> Self {
        Self { tx, tick_rate }
    }

    fn run(&self) -> Result<()> {
        let mut last_tick = Instant::now();

        loop {
            let timeout = self
                .tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(timeout)? {
                match event::read() {
                    Ok(CEvent::Key(KeyEvent {
                        code,
                        kind: KeyEventKind::Press | KeyEventKind::Repeat,
                        modifiers,
                        state: _,
                    })) => self.translate_key_event(code, modifiers, false)?,
                    Ok(CEvent::Key(KeyEvent {
                        code,
                        kind: KeyEventKind::Release,
                        modifiers,
                        state: _,
                    })) => self.translate_key_event(code, modifiers, true)?,
                    Ok(CEvent::Mouse(key)) => self.translate_mouse_event(key)?,
                    Ok(CEvent::Resize(_, _)) => self.send(Event::Resize)?,
                    _ => (),
                }
            }

            if last_tick.elapsed() >= self.tick_rate {
                if self.tx.blocking_send(Event::Tick).is_err() {
                    break;
                }
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    fn translate_key_event(
        &self,
        code: KeyCode,
        modifiers: KeyModifiers,
        is_release: bool,
    ) -> Result<()> {
        let key = match code {
            KeyCode::Backspace => {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    Key::CtrlBackspace
                } else if modifiers.contains(KeyModifiers::ALT) {
                    Key::AltBackspace
                } else {
                    Key::Backspace
                }
            }
            KeyCode::Esc => Key::Esc,
            KeyCode::Up => {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    Key::CtrlUp
                } else {
                    Key::Up
                }
            }
            KeyCode::Down => {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    Key::CtrlDown
                } else {
                    Key::Down
                }
            }
            KeyCode::Left => {
                if modifiers.contains(KeyModifiers::CONTROL)
                    || modifiers.contains(KeyModifiers::SHIFT)
                {
                    Key::CtrlLeft
                } else if modifiers.contains(KeyModifiers::ALT) {
                    Key::AltLeft
                } else {
                    Key::Left
                }
            }
            KeyCode::Right => {
                if modifiers.contains(KeyModifiers::CONTROL)
                    || modifiers.contains(KeyModifiers::SHIFT)
                {
                    Key::CtrlRight
                } else if modifiers.contains(KeyModifiers::ALT) {
                    Key::AltRight
                } else {
                    Key::Right
                }
            }
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Delete => {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    Key::CtrlDelete
                } else if modifiers.contains(KeyModifiers::ALT) {
                    Key::AltDelete
                } else {
                    Key::Delete
                }
            }
            KeyCode::Insert => Key::Insert,
            KeyCode::Tab => Key::Tab,
            KeyCode::Enter => Key::Enter,
            KeyCode::Char(c) => match modifiers {
                KeyModifiers::NONE | KeyModifiers::SHIFT => Key::Char(c),
                KeyModifiers::CONTROL => Key::Ctrl(c),
                KeyModifiers::ALT => Key::Alt(c),
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };

        if is_release {
            self.send(Event::KeyRelease(key))?;
        } else {
            self.send(Event::Input(key))?;
        }

        Ok(())
    }

    fn translate_mouse_event(&self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                self.send(Event::Mouse(mouse))?;
            }
            _ => (),
        }

        Ok(())
    }

    fn send(&self, event: Event) -> Result<()> {
        let _ = self.tx.blocking_send(event);
        Ok(())
    }
}
