//! Text of the built-in Help page. The shortcut listing is generated from the keymap.

pub const HELP_TITLE: &str = "Help";
pub const SHORTCUTS_TITLE: &str = "Keyboard Shortcuts";

pub fn help_text() -> String {
    format!(
        "\
JMX {version} - lightweight GPU text editor
============================================

JMX is a small, fast, dark-themed text editor. Everything can be done with the
keyboard alone, and the common actions also work with just the mouse.

This page is read-only. Close it with Ctrl+W, search it with Ctrl+F.


Two views
---------
The window is split into a left and a right view, each with its own tabs.
Files open in the view that has the cursor; Help and Shortcuts open on the
right. The tab with the blue line on top is where you are typing.

  F6               move the cursor to the other view
  Ctrl+1 / Ctrl+2  move the cursor to the left / right view
  Shift+F6         switch between one and two views (one view shows the view
                   with the cursor; F6 flips to the other one)
  Drag the line    resize the views (mouse)
  Ctrl+F6          center the line again (or double-click it)

Clicking in a view also moves the cursor there; the mouse wheel scrolls the
view under the mouse. A file is open in only one view at a time.


Menu bar
--------
The bar at the very top works with plain mouse clicks:

  File (Alt+F)     New, Open, Open Path, Save, Save As, Close Tab, Quit
  Help  F1         show this page
  Shortcuts  F2    show every keyboard shortcut

In the File menu, Up/Down choose an item, Enter runs it and Esc closes it.
Hover over a button to see what it does in the status bar.


Opening files
-------------
Open (Ctrl+O) shows the file picker:

  Click            select an entry
  Double-click     open a file or enter a folder
  Up/Down, PgUp/PgDn, Home/End    move the selection
  Enter            open the selected file / enter the folder
  Type             filter the list (start with '.' to show hidden files)
  Backspace        edit the filter, or go to the parent folder
  Esc              close the picker (clicking outside it also closes it)

If the filter matches nothing, Enter creates a new file with that name.

Ctrl+Shift+O opens a path prompt instead: type a path, press Tab to complete
it, Enter to open. A path that does not exist opens as a new file.


Tabs and saving
---------------
Every open file is a tab. A dot (●) on the tab and [+] in the status bar mean
unsaved changes. Closing a modified tab or quitting asks whether to save.

Saving keeps the file's original line endings (LF or CRLF) and byte order
mark, and writes to a temporary file first so a crash cannot leave a
half-written file behind.


Prompts
-------
Find, Go to line, Open path and Save as use the prompt line at the bottom of
the window. Esc closes it. While finding, Enter / F3 / Down go to the next
match and Shift+F3 / Up to the previous one. All matches on screen are
highlighted.


Status bar
----------
  EDIT / FIND / OPEN / PICK ...   current mode
  path [+]                        file and modified state
  Ln, Col                         cursor position
  Spaces: 4 / Tabs                indentation used by Tab (detected per file)
  UTF-8, LF / CRLF                encoding and line endings


Command line
------------
  jmx [FILE]...      open files (missing files open as new, unsaved files)
  jmx --help         print the shortcut list
  jmx --version      print the version

  jmx --headless     run without a window (for development, see below)


Logging
-------
Every launch writes a new log file; the previous run's log is kept as
*.prev.log and older logs are deleted automatically.

  Log folder:  {log_dir}
  This run:    {log_file}

Debug builds log at debug level, release builds at info level. Change it with
the JMX_LOG environment variable or --log-level:

  JMX_LOG=trace                  editor at trace (every key and command)
  JMX_LOG=trace,wgpu_hal=info    also GPU driver messages at info
  JMX_LOG=*=info                 all libraries at info

Release builds never contain debug/trace logging. Crashes are written to the
log with a backtrace.


Headless mode (development)
---------------------------
  jmx --headless --script test.jmxs [--size 1100x720] [FILE]...

Runs the editor without a window, rendering offscreen, so it never touches the
desktop. Commands come from the script (or stdin):

  key ctrl+shift+k     type TEXT          move X,Y        click X,Y
  dclick X,Y           press / release X,Y                wheel DY [X,Y]
  resize WxH           shot FILE.png      state           sleep MS     quit

`shot` saves a PNG of the current frame, `state` prints the editor state as
JSON. Headless runs log to jmx-headless.log.
",
        version = env!("CARGO_PKG_VERSION"),
        log_dir =
            crate::logging::default_dir().map_or("(none)".to_string(), |d| d.display().to_string()),
        log_file =
            crate::logging::log_path().map_or("(stderr)".to_string(), |p| p.display().to_string()),
    )
}

pub fn shortcuts_text() -> String {
    format!(
        "Keyboard Shortcuts\n==================\n\n{}\
Mouse: click to place the cursor, drag to select, double-click selects a word,
triple-click selects a line, Shift+click extends the selection, the wheel
scrolls (Shift+wheel scrolls sideways), clicking a tab switches to it.
",
        crate::keymap::listing_text()
    )
}
