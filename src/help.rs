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


Editor and panel
----------------
The left view is the editor: every file opens there. The right view is a
panel for the editor's own pages (Help, Shortcuts; later errors and
settings). The tab with the blue line on top is where the cursor is.

  F6               move the cursor between the editor and the panel
  Ctrl+1 / Ctrl+2  move the cursor to the editor / the panel
  Shift+F6         hide / show the panel (F1, F2 and Ctrl+2 show it again)
  Drag the line    resize the panel (mouse)
  Ctrl+F6          center the line again (or double-click it)

Editing extras
--------------
  Ctrl+K           delete the line
  Ctrl+R           revert: discard all unsaved changes to the file
                   (one undo step, so Ctrl+Z brings them back)

Multiple cursors
----------------
  Shift+Alt+.      select the word under the cursor; press again to add the
                   next occurrence of the selection as another cursor
  Shift+Alt+,      remove the occurrence added last
  Ctrl+Shift+L     select all occurrences at once
  Esc              back to one cursor
Typing, Backspace/Delete, Enter, Tab, arrows/Home/End (with Shift too),
copy, cut and paste act at every cursor. Copying from N cursors and pasting
into N cursors puts one piece at each. Other commands go back to one cursor
first. The Selection menu in the bar has the same commands.

Clicking in a view also moves the cursor there; the mouse wheel scrolls the
view under the mouse. A file is open in only one view at a time.


Workspaces
----------
JMX starts in file mode: open and edit any files. A workspace is a named set
of folders plus the whole editor state (tabs, cursors, views, unsaved edits),
saved as a .jmxws project file.

Workspace menu (Alt+W):
  New Workspace…        give it a name; your open tabs stay open
  Open Workspace…       pick a .jmxws file (or: jmx project.jmxws)
  Save Workspace (As…)  write the project file
  Add Folder…           choose a folder; all files under it are available
  Remove Folder…        take a folder out of the workspace
  Respect .gitignore    hide git-ignored files (on by default)
  Close Workspace       back to file mode

Once a workspace has been saved, it saves itself automatically when you
quit, close it or open another workspace. Unsaved edits are kept inside the
workspace file (your files on disk are not touched) and come back when you
reopen it; Ctrl+Z goes back to the version on disk. Files outside the
workspace folders can still be opened as usual.

Explorer (Ctrl+B show/hide, Ctrl+Shift+E to move the cursor there):
  Up/Down, PgUp/PgDn    move        Right / Left   expand / collapse
  Enter or Space        open file / toggle folder
  a letter              jump to the next name starting with it
  Delete                remove a top-level folder from the workspace
  Esc                   back to the editor
  Mouse: click to open or toggle, drag the right edge to resize, + adds a
  folder.

Quick Open (Ctrl+P): type a few letters of a file name, e.g. appvue, and
press Enter. Searches every file in the workspace folders. Add :N to go to
a line straight away, e.g. appvue:42 (or appvue:42:7 for a column). You
can also pick a file with the arrows first and then type :42.


Search
------
  Ctrl+F           find in this file (Enter / F3 next, Shift+F3 previous)
  Ctrl+Shift+F     Find in Files: search every file in the workspace
                   (in file mode: the current file's folder)
  F8 / Shift+F8    next / previous Find in Files result, panel closed
  Alt+C            option: match case (off = ignore case)
  Alt+O            option: whole word
  Alt+R            option: regular expression
The options apply to Ctrl+F and Find in Files alike; lit chips (Aa ab .*)
show which are on.

In the Find in Files panel:
  Type             the search text; results update as you type
  Tab              switch to the Files filter and back, e.g.
                   *.js, .vue, src/**, !*.spec.js  (! excludes)
  Up/Down, PgUp/PgDn   pick a result      Enter   open it at the match
  Esc              close (Ctrl+Shift+F reopens the last results)
Files with unsaved edits are searched as they are in the editor.


Menu bar
--------
The bar at the very top works with plain mouse clicks:

  Workspace (Alt+W) workspaces, folders, Explorer, Quick Open (see above)
  File (Alt+F)     New, Open, Open Path, Save, Save As, Revert, Close, Quit
  Selection (Alt+S) select all / line, add occurrences (multiple cursors)
  Help  F1         show this page
  Shortcuts  F2    show every keyboard shortcut

In a menu, Up/Down choose an item, Left/Right switch menus, Enter runs it and
Esc closes it.
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
