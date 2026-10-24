//! Key → command table. This is the one place to change bindings; the shortcut
//! listing, `--help` and toolbar tooltips are all generated from it.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::commands::{Command, Group, Motion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Named(NamedKey),
    /// Lowercase character.
    Char(char),
}

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub ctrl: bool,
    /// `None` = shift state does not matter.
    pub shift: Option<bool>,
    pub alt: bool,
    pub key: KeyCode,
    pub cmd: Command,
    /// Shown in the shortcut listing (false for duplicate alternatives).
    pub listed: bool,
}

const fn bind(ctrl: bool, shift: Option<bool>, alt: bool, key: KeyCode, cmd: Command) -> Binding {
    Binding {
        ctrl,
        shift,
        alt,
        key,
        cmd,
        listed: true,
    }
}

const fn hidden(mut b: Binding) -> Binding {
    b.listed = false;
    b
}

const fn n(k: NamedKey) -> KeyCode {
    KeyCode::Named(k)
}

const fn c(ch: char) -> KeyCode {
    KeyCode::Char(ch)
}

const NO: Option<bool> = Some(false);
const YES: Option<bool> = Some(true);
const ANY: Option<bool> = None;

/// Plain and shift-selecting variants of a motion.
macro_rules! motion {
    ($ctrl:expr, $key:expr, $m:expr) => {
        [
            bind($ctrl, NO, false, $key, Command::Move($m, false)),
            bind($ctrl, YES, false, $key, Command::Move($m, true)),
        ]
    };
}

use Command::*;
use NamedKey as NK;

const MOVES: [[Binding; 2]; 12] = [
    motion!(false, n(NK::ArrowLeft), Motion::Left),
    motion!(false, n(NK::ArrowRight), Motion::Right),
    motion!(false, n(NK::ArrowUp), Motion::Up),
    motion!(false, n(NK::ArrowDown), Motion::Down),
    motion!(true, n(NK::ArrowLeft), Motion::WordLeft),
    motion!(true, n(NK::ArrowRight), Motion::WordRight),
    motion!(false, n(NK::Home), Motion::Home),
    motion!(false, n(NK::End), Motion::End),
    motion!(false, n(NK::PageUp), Motion::PageUp),
    motion!(false, n(NK::PageDown), Motion::PageDown),
    motion!(true, n(NK::Home), Motion::DocStart),
    motion!(true, n(NK::End), Motion::DocEnd),
];

const OTHERS: &[Binding] = &[
    // Files
    bind(true, ANY, false, c('n'), NewFile),
    bind(true, NO, false, c('o'), OpenFile),
    bind(true, YES, false, c('o'), OpenPath),
    bind(true, NO, false, c('s'), Save),
    bind(true, YES, false, c('s'), SaveAs),
    bind(true, ANY, false, c('w'), CloseFile),
    bind(true, ANY, false, c('q'), Quit),
    bind(false, ANY, true, c('f'), FileMenu),
    // Split views
    bind(false, NO, false, n(NK::F6), FocusOtherView),
    bind(true, ANY, false, c('1'), FocusLeft),
    bind(true, ANY, false, c('2'), FocusRight),
    bind(false, YES, false, n(NK::F6), ToggleSplit),
    bind(true, ANY, false, n(NK::F6), ResetSplit),
    // Tabs
    bind(true, NO, false, n(NK::Tab), NextTab),
    bind(true, YES, false, n(NK::Tab), PrevTab),
    bind(true, ANY, false, n(NK::PageDown), NextTab),
    bind(true, ANY, false, n(NK::PageUp), PrevTab),
    bind(false, ANY, true, c('1'), GotoTab(0)),
    hidden(bind(false, ANY, true, c('2'), GotoTab(1))),
    hidden(bind(false, ANY, true, c('3'), GotoTab(2))),
    hidden(bind(false, ANY, true, c('4'), GotoTab(3))),
    hidden(bind(false, ANY, true, c('5'), GotoTab(4))),
    hidden(bind(false, ANY, true, c('6'), GotoTab(5))),
    hidden(bind(false, ANY, true, c('7'), GotoTab(6))),
    hidden(bind(false, ANY, true, c('8'), GotoTab(7))),
    bind(false, ANY, true, c('9'), GotoTab(8)),
    // Selection
    bind(true, NO, false, c('a'), SelectAll),
    bind(true, ANY, false, c('l'), SelectLine),
    bind(false, ANY, false, n(NK::Escape), Cancel),
    // Editing
    bind(false, ANY, false, n(NK::Enter), Newline),
    bind(false, NO, false, n(NK::Tab), Indent),
    bind(false, YES, false, n(NK::Tab), Outdent),
    bind(false, ANY, false, n(NK::Backspace), Backspace),
    bind(true, ANY, false, n(NK::Backspace), DeleteWordLeft),
    bind(false, NO, false, n(NK::Delete), Delete),
    bind(true, ANY, false, n(NK::Delete), DeleteWordRight),
    bind(true, YES, false, c('k'), DeleteLine),
    bind(false, ANY, true, n(NK::ArrowUp), MoveLineUp),
    bind(false, ANY, true, n(NK::ArrowDown), MoveLineDown),
    bind(true, ANY, false, c('d'), DuplicateLine),
    // Clipboard & undo
    bind(true, ANY, false, c('c'), Copy),
    hidden(bind(true, NO, false, n(NK::Insert), Copy)),
    bind(true, ANY, false, c('x'), Cut),
    hidden(bind(false, YES, false, n(NK::Delete), Cut)),
    bind(true, ANY, false, c('v'), Paste),
    hidden(bind(false, YES, false, n(NK::Insert), Paste)),
    bind(true, NO, false, c('z'), Undo),
    bind(true, ANY, false, c('y'), Redo),
    bind(true, YES, false, c('z'), Redo),
    // Search & navigation
    bind(true, ANY, false, c('f'), Find),
    bind(false, NO, false, n(NK::F3), FindNext),
    bind(false, YES, false, n(NK::F3), FindPrev),
    bind(true, ANY, false, c('g'), GotoLine),
    // View & help
    bind(true, ANY, false, n(NK::ArrowUp), ScrollUp),
    bind(true, ANY, false, n(NK::ArrowDown), ScrollDown),
    bind(true, ANY, false, c('+'), ZoomIn),
    hidden(bind(true, ANY, false, c('='), ZoomIn)),
    bind(true, ANY, false, c('-'), ZoomOut),
    hidden(bind(true, ANY, false, c('_'), ZoomOut)),
    bind(true, ANY, false, c('0'), ZoomReset),
    bind(false, ANY, false, n(NK::F1), Help),
    bind(false, ANY, false, n(NK::F2), ShowShortcuts),
];

/// All bindings, motions first.
pub fn bindings() -> impl Iterator<Item = &'static Binding> {
    MOVES.iter().flatten().chain(OTHERS.iter())
}

pub fn lookup(mods: ModifiersState, key: &Key) -> Option<Command> {
    let ctrl = mods.control_key() || mods.super_key() && cfg!(target_os = "macos");
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    // AltGr on Windows arrives as Ctrl+Alt; leave those keys to text input.
    if ctrl && alt {
        return None;
    }

    let code = match key {
        Key::Named(k) => KeyCode::Named(*k),
        Key::Character(s) => {
            let mut chars = s.chars();
            let ch = chars.next()?.to_ascii_lowercase();
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(ch)
        }
        _ => return None,
    };
    bindings()
        .find(|b| {
            b.key == code && b.ctrl == ctrl && b.alt == alt && b.shift.is_none_or(|s| s == shift)
        })
        .map(|b| b.cmd)
}

fn key_name(key: KeyCode) -> String {
    match key {
        KeyCode::Char('+') => "Plus".into(),
        KeyCode::Char('-') => "Minus".into(),
        KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
        KeyCode::Named(k) => match k {
            NK::ArrowLeft => "Left".into(),
            NK::ArrowRight => "Right".into(),
            NK::ArrowUp => "Up".into(),
            NK::ArrowDown => "Down".into(),
            NK::PageUp => "PgUp".into(),
            NK::PageDown => "PgDn".into(),
            NK::Escape => "Esc".into(),
            NK::Backspace => "Backspace".into(),
            other => format!("{other:?}"),
        },
    }
}

/// Human-readable key combination, e.g. "Ctrl+Shift+K".
pub fn label(b: &Binding) -> String {
    let mut s = String::new();
    if b.ctrl {
        s.push_str(if cfg!(target_os = "macos") {
            "Cmd+"
        } else {
            "Ctrl+"
        });
    }
    if b.shift == Some(true) {
        s.push_str("Shift+");
    }
    if b.alt {
        s.push_str("Alt+");
    }
    s.push_str(&key_name(b.key));
    s
}

/// Primary shortcut for a command, for tooltips.
pub fn shortcut_for(cmd: Command) -> Option<String> {
    bindings().find(|b| b.listed && b.cmd == cmd).map(label)
}

/// Shortcut listing grouped by section; each entry is (keys, description).
/// Commands with several bindings get them joined ("Ctrl+Y / Ctrl+Shift+Z").
pub fn listing() -> Vec<(Group, Vec<(String, String)>)> {
    Group::ALL
        .iter()
        .map(|&g| {
            let mut rows: Vec<(Command, String)> = Vec::new();
            for b in bindings().filter(|b| b.listed && b.cmd.group() == g) {
                match rows.iter_mut().find(|(c, _)| *c == b.cmd) {
                    Some((_, keys)) => {
                        keys.push_str(" / ");
                        keys.push_str(&label(b));
                    }
                    None => rows.push((b.cmd, label(b))),
                }
            }
            let rows = rows
                .into_iter()
                .map(|(cmd, keys)| (keys, cmd.description()))
                .collect();
            (g, rows)
        })
        .collect()
}

/// Plain-text shortcut table, used by `--help` and the shortcuts tab.
pub fn listing_text() -> String {
    let groups = listing();
    let width = groups
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for (g, rows) in groups {
        if rows.is_empty() {
            continue;
        }
        out.push_str(g.title());
        out.push('\n');
        for (keys, desc) in rows {
            out.push_str(&format!("  {keys:<width$}   {desc}\n"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(b: &Binding) -> ModifiersState {
        let mut m = ModifiersState::empty();
        if b.ctrl {
            m |= ModifiersState::CONTROL;
        }
        if b.shift == Some(true) {
            m |= ModifiersState::SHIFT;
        }
        if b.alt {
            m |= ModifiersState::ALT;
        }
        m
    }

    #[test]
    fn every_binding_round_trips() {
        for b in bindings() {
            let key = match b.key {
                KeyCode::Named(k) => Key::Named(k),
                KeyCode::Char(ch) => Key::Character(ch.to_string().into()),
            };
            assert_eq!(lookup(mods(b), &key), Some(b.cmd), "binding {}", label(b));
        }
    }

    #[test]
    fn shifted_letters_and_altgr() {
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(lookup(cs, &Key::Character("K".into())), Some(DeleteLine));
        assert_eq!(lookup(cs, &Key::Character("S".into())), Some(SaveAs));
        let altgr = ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(lookup(altgr, &Key::Character("7".into())), None);
        assert_eq!(
            lookup(ModifiersState::empty(), &Key::Character("a".into())),
            None
        );
    }

    #[test]
    fn labels_and_listing() {
        if !cfg!(target_os = "macos") {
            assert_eq!(shortcut_for(Save).as_deref(), Some("Ctrl+S"));
            assert_eq!(shortcut_for(DeleteLine).as_deref(), Some("Ctrl+Shift+K"));
        }
        let text = listing_text();
        assert!(text.contains("Save file as"));
        assert!(text.contains("Undo"));
        assert!(text.contains(" / ")); // Redo has two bindings
    }
}
