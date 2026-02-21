use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::config::PrefixKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Command,
}

impl Mode {
    pub fn status_text(&self, terminal_name: &str, prefix: &PrefixKey) -> String {
        match self {
            Mode::Normal => {
                let key_name = prefix.display_name();
                format!(" [NORMAL] {terminal_name} | {key_name}: command mode")
            }
            Mode::Command => {
                " [COMMAND] h/j/k/l:nav s:split-h v:split-v n:new x:close Esc:back".into()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SendToTerminal(Vec<u8>),
    PaneSplitHorizontal,
    PaneSplitVertical,
    PaneNavigate(Direction),
    PaneResize(Direction),
    PaneClose,
    TerminalNew,
    ViewList,
    ViewSwitch(usize),
    Detach,
    ModeChanged(Mode),
    None,
}

pub struct InputHandler {
    mode: Mode,
    prefix: PrefixKey,
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            prefix: PrefixKey::default(),
        }
    }

    pub fn with_prefix(prefix: PrefixKey) -> Self {
        Self {
            mode: Mode::Normal,
            prefix,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn handle_event(&mut self, event: &Event) -> Action {
        let Event::Key(key) = event else {
            return Action::None;
        };
        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Command => self.handle_command(key),
        }
    }

    fn handle_normal(&mut self, key: &KeyEvent) -> Action {
        if key.code == self.prefix.code && key.modifiers.contains(self.prefix.modifiers) {
            self.mode = Mode::Command;
            return Action::ModeChanged(Mode::Command);
        }
        match key_event_to_bytes(key) {
            Some(bytes) => Action::SendToTerminal(bytes),
            _ => Action::None,
        }
    }

    fn handle_command(&mut self, key: &KeyEvent) -> Action {
        let action = match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return Action::ModeChanged(Mode::Normal);
            }
            KeyCode::Char('h') => Action::PaneNavigate(Direction::Left),
            KeyCode::Char('j') => Action::PaneNavigate(Direction::Down),
            KeyCode::Char('k') => Action::PaneNavigate(Direction::Up),
            KeyCode::Char('x') => Action::PaneClose,
            KeyCode::Char('l') => Action::PaneNavigate(Direction::Right),
            KeyCode::Char('s') => Action::PaneSplitHorizontal,
            KeyCode::Char('v') => Action::PaneSplitVertical,
            KeyCode::Char('n') => Action::TerminalNew,
            KeyCode::Char('w') => Action::ViewList,
            KeyCode::Char('d') => Action::Detach,
            KeyCode::Char(c @ '1'..='9') => Action::ViewSwitch(c as usize - '0' as usize),
            KeyCode::Char('H') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::PaneResize(Direction::Left)
            }
            KeyCode::Char('J') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::PaneResize(Direction::Down)
            }
            KeyCode::Char('K') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::PaneResize(Direction::Up)
            }
            KeyCode::Char('L') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::PaneResize(Direction::Right)
            }
            _ => return Action::None,
        };
        self.mode = Mode::Normal;
        action
    }
}

pub fn key_event_to_bytes(event: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Char(c) if ctrl => {
            let byte = (c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1);
            Some(vec![byte])
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            Some(s.as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![127]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn normal_mode_forwards_chars() {
        let mut h = InputHandler::new();
        assert_eq!(h.mode(), Mode::Normal);
        let action = h.handle_event(&key(KeyCode::Char('a')));
        assert_eq!(action, Action::SendToTerminal(b"a".to_vec()));
    }

    #[test]
    fn ctrl_a_enters_command_mode() {
        let mut h = InputHandler::new();
        let action = h.handle_event(&ctrl('a'));
        assert_eq!(action, Action::ModeChanged(Mode::Command));
        assert_eq!(h.mode(), Mode::Command);
    }

    #[test]
    fn esc_returns_to_normal() {
        let mut h = InputHandler::new();
        h.handle_event(&ctrl('a')); // enter command
        let action = h.handle_event(&key(KeyCode::Esc));
        assert_eq!(action, Action::ModeChanged(Mode::Normal));
        assert_eq!(h.mode(), Mode::Normal);
    }

    #[test]
    fn command_mode_unmapped_key_returns_none() {
        let mut h = InputHandler::new();
        h.handle_event(&ctrl('a'));
        let action = h.handle_event(&key(KeyCode::Char('z')));
        assert_eq!(action, Action::None);
        // Intentional: unmapped keys stay in Command mode so user can retry
        assert_eq!(h.mode(), Mode::Command);
    }

    #[test]
    fn normal_mode_forwards_enter() {
        let mut h = InputHandler::new();
        let action = h.handle_event(&key(KeyCode::Enter));
        assert_eq!(action, Action::SendToTerminal(vec![b'\r']));
    }

    #[test]
    fn normal_mode_forwards_ctrl_c() {
        let mut h = InputHandler::new();
        let action = h.handle_event(&ctrl('c'));
        assert_eq!(action, Action::SendToTerminal(vec![3]));
    }

    fn cmd(h: &mut InputHandler, code: KeyCode) -> Action {
        h.handle_event(&ctrl('a'));
        h.handle_event(&key(code))
    }

    #[test]
    fn command_navigate() {
        let mut h = InputHandler::new();
        assert_eq!(
            cmd(&mut h, KeyCode::Char('h')),
            Action::PaneNavigate(Direction::Left)
        );
        assert_eq!(
            cmd(&mut h, KeyCode::Char('j')),
            Action::PaneNavigate(Direction::Down)
        );
        assert_eq!(
            cmd(&mut h, KeyCode::Char('k')),
            Action::PaneNavigate(Direction::Up)
        );
        assert_eq!(
            cmd(&mut h, KeyCode::Char('l')),
            Action::PaneNavigate(Direction::Right)
        );
    }

    #[test]
    fn command_close() {
        let mut h = InputHandler::new();
        assert_eq!(cmd(&mut h, KeyCode::Char('x')), Action::PaneClose);
    }

    #[test]
    fn command_split() {
        let mut h = InputHandler::new();
        assert_eq!(cmd(&mut h, KeyCode::Char('s')), Action::PaneSplitHorizontal);
        assert_eq!(cmd(&mut h, KeyCode::Char('v')), Action::PaneSplitVertical);
    }

    #[test]
    fn command_terminal_ops() {
        let mut h = InputHandler::new();
        assert_eq!(cmd(&mut h, KeyCode::Char('n')), Action::TerminalNew);
    }

    #[test]
    fn command_resize() {
        let mut h = InputHandler::new();
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        h.handle_event(&ctrl('a'));
        assert_eq!(h.handle_event(&ev), Action::PaneResize(Direction::Left));
    }

    #[test]
    fn command_view_switch() {
        let mut h = InputHandler::new();
        assert_eq!(cmd(&mut h, KeyCode::Char('3')), Action::ViewSwitch(3));
    }

    #[test]
    fn command_returns_to_normal_after_action() {
        let mut h = InputHandler::new();
        cmd(&mut h, KeyCode::Char('s'));
        assert_eq!(h.mode(), Mode::Normal);
    }
}
