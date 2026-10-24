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
