/// Cursor motions; combined with a `select` flag in [`Command::Move`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

impl Motion {
    fn description(self) -> &'static str {
        match self {
            Motion::Left => "character left",
            Motion::Right => "character right",
            Motion::Up => "line up",
            Motion::Down => "line down",
            Motion::WordLeft => "word left",
            Motion::WordRight => "word right",
            Motion::Home => "line start (first non-blank / column 1)",
            Motion::End => "line end",
            Motion::PageUp => "page up",
            Motion::PageDown => "page down",
            Motion::DocStart => "start of file",
            Motion::DocEnd => "end of file",
        }
    }
}

/// Every editor action. Key bindings map onto these in `keymap.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Move(Motion, bool),
    SelectAll,
    SelectLine,
    /// Escape: close the prompt or collapse the selection.
    Cancel,

    Newline,
    Indent,
    Outdent,
    Backspace,
    Delete,
    DeleteWordLeft,
    DeleteWordRight,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    DuplicateLine,

    Copy,
    Cut,
    Paste,
    Undo,
    Redo,

    NewFile,
    /// File picker overlay.
    OpenFile,
    /// Path prompt with Tab completion.
    OpenPath,
    Save,
    SaveAs,
    CloseFile,
    NextTab,
    PrevTab,
    GotoTab(usize),

    Find,
    FindNext,
    FindPrev,
    GotoLine,

    ScrollUp,
    ScrollDown,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Help,
    ShowShortcuts,
    Quit,
}

/// Sections of the shortcut listing, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    File,
    Tabs,
    Move,
    Select,
    Edit,
    Clipboard,
    Search,
    View,
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::File,
        Group::Tabs,
        Group::Move,
        Group::Select,
        Group::Edit,
        Group::Clipboard,
        Group::Search,
        Group::View,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Group::File => "Files",
            Group::Tabs => "Tabs",
            Group::Move => "Cursor movement",
            Group::Select => "Selection",
            Group::Edit => "Editing",
            Group::Clipboard => "Clipboard & undo",
            Group::Search => "Search & navigation",
            Group::View => "View & help",
        }
    }
}

impl Command {
    pub fn description(self) -> String {
        use Command::*;
        let s = match self {
            Move(m, false) => return format!("Move {}", m.description()),
            Move(m, true) => return format!("Select {}", m.description()),
            GotoTab(i) => return format!("Go to tab {}", i + 1),
            SelectAll => "Select all",
            SelectLine => "Select line (repeat to extend)",
            Cancel => "Close prompt / clear selection",
            Newline => "New line (keeps indentation)",
            Indent => "Indent / insert tab",
            Outdent => "Outdent",
            Backspace => "Delete character left",
            Delete => "Delete character right",
            DeleteWordLeft => "Delete word left",
            DeleteWordRight => "Delete word right",
            DeleteLine => "Delete line",
            MoveLineUp => "Move line up",
            MoveLineDown => "Move line down",
            DuplicateLine => "Duplicate line",
            Copy => "Copy (whole line if nothing selected)",
            Cut => "Cut (whole line if nothing selected)",
            Paste => "Paste",
            Undo => "Undo",
            Redo => "Redo",
            NewFile => "New file",
            OpenFile => "Open file (file picker)",
            OpenPath => "Open file by typing a path",
            Save => "Save file",
            SaveAs => "Save file as",
            CloseFile => "Close tab",
            NextTab => "Next tab",
            PrevTab => "Previous tab",
            Find => "Find",
            FindNext => "Find next",
            FindPrev => "Find previous",
            GotoLine => "Go to line",
            ScrollUp => "Scroll up",
            ScrollDown => "Scroll down",
            ZoomIn => "Increase font size",
            ZoomOut => "Decrease font size",
            ZoomReset => "Reset font size",
            Help => "Help",
            ShowShortcuts => "Keyboard shortcuts",
            Quit => "Quit",
        };
        s.to_string()
    }

    pub fn group(self) -> Group {
        use Command::*;
        match self {
            Move(_, false) => Group::Move,
            Move(_, true) | SelectAll | SelectLine | Cancel => Group::Select,
            Newline | Indent | Outdent | Backspace | Delete | DeleteWordLeft | DeleteWordRight
            | DeleteLine | MoveLineUp | MoveLineDown | DuplicateLine => Group::Edit,
            Copy | Cut | Paste | Undo | Redo => Group::Clipboard,
            NewFile | OpenFile | OpenPath | Save | SaveAs | CloseFile | Quit => Group::File,
            NextTab | PrevTab | GotoTab(_) => Group::Tabs,
            Find | FindNext | FindPrev | GotoLine => Group::Search,
            ScrollUp | ScrollDown | ZoomIn | ZoomOut | ZoomReset | Help | ShowShortcuts => {
                Group::View
            }
        }
    }
}
