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
    /// Reload the file from disk, discarding unsaved changes (one undo step).
    RevertFile,
    CloseFile,
    NextTab,
    PrevTab,
    GotoTab(usize),

    Find,
    FindNext,
    FindPrev,
    GotoLine,
    /// Grep-style search across the workspace (overlay panel).
    FindInFiles,
    /// Jump to the next / previous Find in Files result.
    NextResult,
    PrevResult,
    ToggleCase,
    ToggleWholeWord,
    ToggleRegex,

    ScrollUp,
    ScrollDown,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Help,
    ShowShortcuts,
    /// Move the cursor to the other view (left/right).
    FocusOtherView,
    FocusLeft,
    FocusRight,
    /// Switch between one and two views.
    ToggleSplit,
    /// Put the divider between the views back in the center.
    ResetSplit,
    /// Open the File menu (handled by the session, which owns menu state).
    FileMenu,
    /// Open the Selection menu (handled by the session).
    SelectionMenu,
    /// Multi-cursor: select the word, or add the next occurrence of the selection.
    AddNextOccurrence,
    /// Multi-cursor: drop the most recently added selection.
    RemoveLastOccurrence,
    /// Multi-cursor: select every occurrence of the selection.
    SelectAllOccurrences,
    /// Open the Workspace menu (handled by the session).
    WorkspaceMenu,
    /// Fuzzy search over all workspace files.
    QuickOpen,
    ToggleExplorer,
    FocusExplorer,
    NewWorkspace,
    OpenWorkspace,
    SaveWorkspace,
    SaveWorkspaceAs,
    AddFolder,
    RemoveFolder,
    ToggleGitignore,
    CloseWorkspace,
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
    Views,
    Workspace,
    View,
}

impl Group {
    pub const ALL: [Group; 10] = [
        Group::File,
        Group::Tabs,
        Group::Move,
        Group::Select,
        Group::Edit,
        Group::Clipboard,
        Group::Search,
        Group::Views,
        Group::Workspace,
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
            Group::Views => "Split views",
            Group::Workspace => "Workspace",
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
            RevertFile => "Revert file: discard all unsaved changes",
            CloseFile => "Close tab",
            NextTab => "Next tab",
            PrevTab => "Previous tab",
            Find => "Find",
            FindNext => "Find next",
            FindPrev => "Find previous",
            GotoLine => "Go to line",
            FindInFiles => "Find in files",
            NextResult => "Next Find in Files result",
            PrevResult => "Previous Find in Files result",
            ToggleCase => "Search option: match case",
            ToggleWholeWord => "Search option: whole word",
            ToggleRegex => "Search option: regular expression",
            ScrollUp => "Scroll up",
            ScrollDown => "Scroll down",
            ZoomIn => "Increase font size",
            ZoomOut => "Decrease font size",
            ZoomReset => "Reset font size",
            Help => "Help",
            ShowShortcuts => "Keyboard shortcuts",
            FocusOtherView => "Move cursor to the other view",
            FocusLeft => "Move cursor to the left view",
            FocusRight => "Move cursor to the right view",
            ToggleSplit => "Toggle one / two views",
            ResetSplit => "Center the divider between views",
            FileMenu => "Open the File menu",
            SelectionMenu => "Open the Selection menu",
            AddNextOccurrence => "Add next occurrence (multi-cursor)",
            RemoveLastOccurrence => "Remove last added occurrence",
            SelectAllOccurrences => "Select all occurrences (multi-cursor)",
            WorkspaceMenu => "Open the Workspace menu",
            QuickOpen => "Quick Open: find a file (name:line goes to a line)",
            ToggleExplorer => "Show / hide the Explorer",
            FocusExplorer => "Move cursor to the Explorer",
            NewWorkspace => "New workspace",
            OpenWorkspace => "Open workspace",
            SaveWorkspace => "Save workspace",
            SaveWorkspaceAs => "Save workspace as",
            AddFolder => "Add folder to workspace",
            RemoveFolder => "Remove folder from workspace",
            ToggleGitignore => "Respect .gitignore",
            CloseWorkspace => "Close workspace",
            Quit => "Quit",
        };
        s.to_string()
    }

    pub fn group(self) -> Group {
        use Command::*;
        match self {
            Move(_, false) => Group::Move,
            Move(_, true)
            | SelectAll
            | SelectLine
            | Cancel
            | SelectionMenu
            | AddNextOccurrence
            | RemoveLastOccurrence
            | SelectAllOccurrences => Group::Select,
            Newline | Indent | Outdent | Backspace | Delete | DeleteWordLeft | DeleteWordRight
            | DeleteLine | MoveLineUp | MoveLineDown | DuplicateLine => Group::Edit,
            Copy | Cut | Paste | Undo | Redo => Group::Clipboard,
            NewFile | OpenFile | OpenPath | Save | SaveAs | RevertFile | CloseFile | Quit
            | FileMenu => Group::File,
            FocusOtherView | FocusLeft | FocusRight | ToggleSplit | ResetSplit => Group::Views,
            WorkspaceMenu | QuickOpen | ToggleExplorer | FocusExplorer | NewWorkspace
            | OpenWorkspace | SaveWorkspace | SaveWorkspaceAs | AddFolder | RemoveFolder
            | ToggleGitignore | CloseWorkspace => Group::Workspace,
            NextTab | PrevTab | GotoTab(_) => Group::Tabs,
            Find | FindNext | FindPrev | GotoLine | FindInFiles | NextResult | PrevResult
            | ToggleCase | ToggleWholeWord | ToggleRegex => Group::Search,
            ScrollUp | ScrollDown | ZoomIn | ZoomOut | ZoomReset | Help | ShowShortcuts => {
                Group::View
            }
        }
    }
}
