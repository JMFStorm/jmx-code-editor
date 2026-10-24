//! End-to-end tests: run the real binary in `--headless` mode with a script.
//! Skipped (with a note) when the machine has no usable GPU adapter.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXIT_NO_GPU: i32 = 3;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jmx-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `jmx --headless` with `script`; `None` means no GPU (test should be skipped).
fn run(dir: &Path, script: &str, files: &[&Path]) -> Option<Output> {
    let script_path = dir.join("test.jmxs");
    fs::write(&script_path, script).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
        .arg("--headless")
        .arg("--size=640x400")
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .arg("--script")
        .arg(&script_path)
        .args(files)
        .output()
        .expect("failed to start jmx");
    if out.status.code() == Some(EXIT_NO_GPU) {
        eprintln!("skipping: no GPU adapter available for headless rendering");
        return None;
    }
    assert!(
        out.status.success(),
        "jmx failed: {}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    Some(out)
}

#[test]
fn edit_save_state_and_screenshot() {
    let dir = temp_dir("edit");
    let file = dir.join("hello.txt");
    fs::write(&file, "abc\r\ndef\r\n").unwrap();
    let shot = dir.join("shots/frame.png");
    let script = format!(
        "# edit the first line and save\n\
         key end\n\
         type  XYZ\n\
         key ctrl+s\n\
         state\n\
         shot {}\n\
         quit\n",
        shot.display()
    );
    let Some(out) = run(&dir, &script, &[&file]) else {
        return;
    };
    // CRLF line endings survive the edit.
    assert_eq!(fs::read_to_string(&file).unwrap(), "abc XYZ\r\ndef\r\n");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let state = stdout
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("state line");
    assert!(state.contains("\"dirty\":false"), "{state}");
    assert!(state.contains("\"line_text\":\"abc XYZ\""), "{state}");
    assert!(state.contains("\"line_ending\":\"CRLF\""), "{state}");
    assert!(state.contains("\"col\":8"), "{state}");

    // PNG signature and IHDR size.
    let png = fs::read(&shot).expect("screenshot written");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (640, 400));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn script_errors_report_line_numbers() {
    let dir = temp_dir("err");
    fs::write(dir.join("test.jmxs"), "state\nfrobnicate\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
        .args(["--headless", "--size=320x200", "--log-dir"])
        .arg(dir.join("logs"))
        .arg("--script")
        .arg(dir.join("test.jmxs"))
        .output()
        .unwrap();
    if out.status.code() == Some(EXIT_NO_GPU) {
        return;
    }
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error line 2: unknown command `frobnicate`"),
        "{stderr}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn each_launch_rotates_logs() {
    let dir = temp_dir("logs");
    for _ in 0..3 {
        if run(&dir, "state\nquit\n", &[]).is_none() {
            return;
        }
    }
    let mut names: Vec<String> = fs::read_dir(dir.join("logs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["jmx-headless.log", "jmx-headless.prev.log"]);
    let log = fs::read_to_string(dir.join("logs/jmx-headless.log")).unwrap();
    assert!(log.starts_with("# jmx "), "{log}");
    assert!(log.contains("headless session 640x400"), "{log}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn split_views_focus_and_toggle() {
    let dir = temp_dir("split");
    let file = dir.join("a.txt");
    fs::write(&file, "left\n").unwrap();
    let script = "state\n\
                  key f6\n\
                  type typed into help\n\
                  state\n\
                  key shift+f6\n\
                  state\n\
                  key ctrl+f6\n\
                  key f2\n\
                  state\n\
                  quit\n";
    let Some(out) = run(&dir, script, &[&file]) else {
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let states: Vec<&str> = stdout.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(states.len(), 4, "{stdout}");
    // Startup: file on the left with focus, Help on the right.
    assert!(
        states[0].contains("\"focus\":0,\"split\":true"),
        "{}",
        states[0]
    );
    assert!(states[0].contains("\"title\":\"Help\""), "{}", states[0]);
    // F6 moves to Help, which refuses typing.
    assert!(states[1].contains("\"focus\":1"), "{}", states[1]);
    assert!(states[1].contains("\"read_only\":true"), "{}", states[1]);
    assert!(
        states[1].contains("\"message\":\"Read-only\""),
        "{}",
        states[1]
    );
    // Shift+F6: one view.
    assert!(states[2].contains("\"split\":false"), "{}", states[2]);
    // F2 opens Shortcuts in the right view.
    assert!(
        states[3].contains("\"title\":\"Keyboard Shortcuts\""),
        "{}",
        states[3]
    );
    assert!(states[3].contains("\"split_ratio\":0.500"), "{}", states[3]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "left\n");
    fs::remove_dir_all(&dir).unwrap();
}

fn states(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(str::to_string)
        .collect()
}

#[test]
fn workspace_create_save_and_reopen() {
    let dir = temp_dir("ws");
    let proj = dir.join("proj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::create_dir_all(proj.join("node_modules/lib")).unwrap();
    fs::write(proj.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(proj.join("src/main.js"), "let a = 1;\n").unwrap();
    fs::write(proj.join("node_modules/lib/x.js"), "x\n").unwrap();
    let start = proj.join("src/main.js");

    // Run 1: Workspace menu → New (name), Add Folder via the picker (go up from src/ and
    // take the "Add this folder" row), Quick Open, edit without saving, Save Workspace As.
    let script = "key alt+w\n\
                  key enter\n\
                  type demo\n\
                  key enter\n\
                  key alt+w\n\
                  key down\nkey down\nkey down\nkey down\n\
                  key enter\n\
                  key backspace\n\
                  key home\n\
                  key enter\n\
                  key ctrl+p\n\
                  type mainjs\n\
                  key enter\n\
                  key end\n\
                  type  // changed\n\
                  key alt+w\n\
                  key down\nkey down\nkey down\n\
                  key enter\n\
                  key enter\n\
                  state\n\
                  quit\n";
    let Some(out) = run(&dir, script, &[&start]) else {
        return;
    };
    let s = states(&out);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].contains("\"name\":\"demo\""), "{}", s[0]);
    assert!(s[0].contains("demo.jmxws"), "{}", s[0]);
    assert!(s[0].contains("\"dirty\":true"), "{}", s[0]);
    assert!(
        !s[0].contains("node_modules"),
        "gitignored folder shown: {}",
        s[0]
    );
    let ws_file = dir.join("demo.jmxws");
    assert!(ws_file.exists(), "workspace file not written");
    // The edit lives in the workspace, not on disk.
    assert_eq!(fs::read_to_string(&start).unwrap(), "let a = 1;\n");

    // Run 2: reopen from the command line; tab, cursor and unsaved edit come back.
    let Some(out) = run(&dir, "state\nquit\n", &[&ws_file]) else {
        return;
    };
    let s = states(&out);
    assert!(
        s[0].contains("\"line_text\":\"let a = 1; // changed\""),
        "{}",
        s[0]
    );
    assert!(s[0].contains("\"dirty\":true"), "{}", s[0]);
    assert!(
        s[0].contains("\"workspace\":{\"name\":\"demo\""),
        "{}",
        s[0]
    );
    assert!(s[0].contains("\"rows\":[\"proj\",\"  src\""), "{}", s[0]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn quick_open_line_and_find_in_files() {
    let dir = temp_dir("search");
    let proj = dir.join("proj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(proj.join("src/csv.js"), "a\nb\nparse(Needle)\nneedle\n").unwrap();
    fs::write(proj.join("src/app.vue"), "<p>needle</p>\n").unwrap();
    fs::write(proj.join("src/app.js"), "one\ntwo\nthree\n").unwrap();
    let ws = dir.join("p.jmxws");
    fs::write(
        &ws,
        r#"{"version":1,"name":"p","folders":["proj"],"respect_gitignore":true}"#,
    )
    .unwrap();
    let script = "key ctrl+p\n\
                  type csv:3:7\n\
                  key enter\n\
                  state\n\
                  key ctrl+shift+f\n\
                  type needle\n\
                  state\n\
                  key alt+c\n\
                  state\n\
                  key tab\n\
                  type .vue\n\
                  state\n\
                  key enter\n\
                  state\n\
                  key ctrl+shift+f\n\
                  key tab\n\
                  key backspace\n\
                  key backspace\n\
                  key backspace\n\
                  key backspace\n\
                  key alt+c\n\
                  key esc\n\
                  key f8\n\
                  state\n\
                  key f8\n\
                  state\n\
                  quit\n";
    let Some(out) = run(&dir, script, &[&ws]) else {
        return;
    };
    let s = states(&out);
    assert_eq!(s.len(), 7, "{s:?}");
    // Quick Open with :line:col.
    assert!(s[0].contains("src/csv.js\""), "{}", s[0]);
    assert!(s[0].contains("\"line\":3,\"col\":7"), "{}", s[0]);
    // Ignore case: 3 matches in 2 files; match case: 2 in 2 files.
    assert!(s[1].contains("\"files\":2,\"matches\":3"), "{}", s[1]);
    assert!(s[2].contains("\"case\":true"), "{}", s[2]);
    assert!(s[2].contains("\"files\":2,\"matches\":2"), "{}", s[2]);
    // File filter.
    assert!(s[3].contains("\"files\":1,\"matches\":1"), "{}", s[3]);
    // Enter opens at the match, selected.
    assert!(s[4].contains("src/app.vue\""), "{}", s[4]);
    assert!(s[4].contains("\"selection\":[3,9]"), "{}", s[4]);
    assert!(s[4].contains("\"open\":false"), "{}", s[4]);
    // F8 steps through results with the panel closed (filter cleared, ignore case again).
    // The first F8 opens the first result, the next one moves on.
    assert!(s[5].contains("\"selected\":\"src/app.vue:1\""), "{}", s[5]);
    assert!(s[5].contains("Result 1 of 3"), "{}", s[5]);
    assert!(s[6].contains("\"selected\":\"src/csv.js:3\""), "{}", s[6]);
    assert!(s[6].contains("\"selection\":[10,16]"), "{}", s[6]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn multi_cursor_revert_and_editor_view() {
    let dir = temp_dir("multi");
    let file = dir.join("m.txt");
    fs::write(&file, "foo bar foo baz foo\n").unwrap();
    let other = dir.join("other.txt");
    fs::write(&other, "other\n").unwrap();
    let script = "key shift+alt+.\n\
         key shift+alt+.\n\
         key shift+alt+.\n\
         state\n\
         type X\n\
         state\n\
         key ctrl+r\n\
         state\n\
         key ctrl+z\n\
         state\n\
         key esc\n\
         key ctrl+k\n\
         state\n\
         key f6\n\
         key ctrl+o\n\
         type other.txt\n\
         key enter\n\
         state\n\
         quit\n";
    let Some(out) = run(&dir, script, &[&file]) else {
        return;
    };
    let s = states(&out);
    assert_eq!(s.len(), 6, "{s:?}");
    assert!(s[0].contains("\"cursors\":3"), "{}", s[0]);
    assert!(s[1].contains("\"line_text\":\"X bar X baz X\""), "{}", s[1]);
    // Revert: back to the disk text and clean; undo brings the edit back.
    assert!(
        s[2].contains("\"line_text\":\"foo bar foo baz foo\""),
        "{}",
        s[2]
    );
    assert!(s[2].contains("\"dirty\":false"), "{}", s[2]);
    assert!(s[3].contains("\"line_text\":\"X bar X baz X\""), "{}", s[3]);
    assert!(s[3].contains("\"dirty\":true"), "{}", s[3]);
    // Ctrl+K deletes the line.
    assert!(s[4].contains("\"line_text\":\"\""), "{}", s[4]);
    assert!(s[4].contains("\"cursors\":1"), "{}", s[4]);
    // A file opened from the panel lands in the editor view.
    assert!(s[5].contains("\"focus\":0"), "{}", s[5]);
    assert!(s[5].contains("\"title\":\"other.txt\""), "{}", s[5]);
    assert_eq!(fs::read_to_string(&file).unwrap(), "foo bar foo baz foo\n");
    fs::remove_dir_all(&dir).unwrap();
}
