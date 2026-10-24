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

#[test]
fn reopens_the_last_workspace() {
    let dir = temp_dir("restore");
    fs::create_dir_all(dir.join("proj")).unwrap();
    fs::write(dir.join("proj/a.js"), "const a = 1;\n").unwrap();
    let ws = dir.join("p.jmxws");
    fs::write(
        &ws,
        r#"{"version":1,"name":"p","folders":["proj"],"respect_gitignore":true}"#,
    )
    .unwrap();
    let state = dir.join("state.json");
    let state_arg = format!("--state-file={}", state.display());
    let run_with = |script: &str, files: &[&Path]| {
        let script_path = dir.join("restore.jmxs");
        fs::write(&script_path, script).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
            .args(["--headless", "--size=640x400", &state_arg])
            .arg("--log-dir")
            .arg(dir.join("logs"))
            .arg("--script")
            .arg(&script_path)
            .args(files)
            .output()
            .expect("failed to start jmx");
        if out.status.code() == Some(EXIT_NO_GPU) {
            return None;
        }
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(out)
    };
    // Opening a workspace remembers it.
    let Some(_) = run_with("key ctrl+p\ntype a.js\nkey enter\nquit\n", &[&ws]) else {
        return;
    };
    assert!(fs::read_to_string(&state).unwrap().contains("p.jmxws"));
    // No files given: the workspace comes back, with its tab.
    let out = run_with("state\nquit\n", &[]).unwrap();
    let s = states(&out);
    assert!(s[0].contains("\"name\":\"p\""), "{}", s[0]);
    assert!(s[0].contains("\"title\":\"a.js\""), "{}", s[0]);
    assert!(s[0].contains("\"lang\":\"JavaScript\""), "{}", s[0]);
    // With a file on the command line the workspace is not reopened.
    let out = run_with("state\nquit\n", &[&dir.join("proj/a.js")]).unwrap();
    assert!(states(&out)[0].contains("\"workspace\":null"));
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn ctrl_s_twice_formats_and_saves() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("format");
    // A stand-in for Prettier: uppercases its input.
    let fake = dir.join("fake-prettier");
    fs::write(&fake, "#!/bin/sh\ntr a-z A-Z\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let js = dir.join("a.js");
    fs::write(&js, "let x = 1\n").unwrap();
    let cs = dir.join("B.cs");
    fs::write(&cs, "class B { }   \n\n\n").unwrap();
    let script = "key end\n\
                  type ;\n\
                  key ctrl+s\n\
                  state\n\
                  key ctrl+s\n\
                  key ctrl+s\n\
                  state\n\
                  type // more\n\
                  key shift+alt+f\n\
                  state\n\
                  key ctrl+tab\n\
                  key ctrl+s\n\
                  key ctrl+s\n\
                  state\n\
                  quit\n";
    let script_path = dir.join("fmt.jmxs");
    fs::write(&script_path, script).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
        .args(["--headless", "--size=640x400"])
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .arg("--script")
        .arg(&script_path)
        .arg(&js)
        .arg(&cs)
        .env("JMX_PRETTIER", &fake)
        .output()
        .expect("failed to start jmx");
    if out.status.code() == Some(EXIT_NO_GPU) {
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = states(&out);
    // One Ctrl+S: saved as typed.
    assert!(s[0].contains("\"line_text\":\"let x = 1;\""), "{}", s[0]);
    // Ctrl held, S S: formatted and saved.
    assert!(s[1].contains("\"line_text\":\"LET X = 1;\""), "{}", s[1]);
    assert!(s[1].contains("\"dirty\":false"), "{}", s[1]);
    // Shift+Alt+F: formatted, not saved.
    assert!(
        s[2].contains("\"line_text\":\"LET X = 1;// MORE\""),
        "{}",
        s[2]
    );
    assert!(s[2].contains("\"dirty\":true"), "{}", s[2]);
    assert_eq!(fs::read_to_string(&js).unwrap(), "LET X = 1;\n");
    // C#: basic cleanup and save.
    assert_eq!(fs::read_to_string(&cs).unwrap(), "class B { }\n");
    assert!(s[3].contains("\"dirty\":false"), "{}", s[3]);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn eslint_problems_page_and_navigation() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("eslint");
    let dir = fs::canonicalize(&dir).unwrap();
    fs::write(dir.join("eslint.config.js"), "export default [];\n").unwrap();
    let a = dir.join("a.js");
    let b = dir.join("b.js");
    fs::write(&a, "const a = 1;\nlet x;\nfoo();\nbar();\n").unwrap();
    fs::write(&b, "b();\n").unwrap();
    // ESLint stand-in: one warning for a --stdin run, the whole project otherwise.
    let full = format!(
        r#"[{{"filePath":"{a}","messages":[{{"ruleId":"no-undef","severity":2,"message":"'foo' is not defined.","line":3,"column":1,"endLine":3,"endColumn":4}},{{"ruleId":"no-unused-vars","severity":1,"message":"'x' is never used.","line":2,"column":5}}]}},{{"filePath":"{b}","messages":[{{"ruleId":"no-undef","severity":1,"message":"'b' is not defined.","line":1,"column":1}}]}}]"#,
        a = a.display(),
        b = b.display()
    );
    let fake = dir.join("fake-eslint");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncat > /dev/null\ncase \"$*\" in\n  *--stdin*) eval last=\\${{$#}}; printf '[{{\"filePath\":\"%s\",\"messages\":[{{\"ruleId\":\"semi\",\"severity\":1,\"message\":\"Missing semicolon.\",\"line\":1,\"column\":5}}]}}]\\n' \"$last\";;\n  *) printf '%s\\n' '{full}';;\nesac\nexit 1\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let shot = dir.join("problems.png");
    let script = format!(
        "state\n\
         key ctrl+shift+m\n\
         state\n\
         key down\nkey down\nkey down\nkey down\n\
         state\n\
         shot {}\n\
         key enter\n\
         state\n\
         key f4\n\
         state\n\
         quit\n",
        shot.display()
    );
    let script_path = dir.join("lint.jmxs");
    fs::write(&script_path, script).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
        .args(["--headless", "--size=900x500"])
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .arg("--script")
        .arg(&script_path)
        .arg(&a)
        .env("JMX_ESLINT", &fake)
        .output()
        .expect("failed to start jmx");
    if out.status.code() == Some(EXIT_NO_GPU) {
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = states(&out);
    // Opening a.js linted it.
    assert!(
        s[0].contains("\"problems\":{\"errors\":0,\"warnings\":1,\"files\":1"),
        "{}",
        s[0]
    );
    // Ctrl+Shift+M: the whole project, with the Problems page in the panel.
    assert!(
        s[1].contains("\"problems\":{\"errors\":1,\"warnings\":2,\"files\":2"),
        "{}",
        s[1]
    );
    assert!(s[1].contains("\"focus\":1"), "{}", s[1]);
    assert!(s[1].contains("\"title\":\"Problems\""), "{}", s[1]);
    // Down to the first problem row: focus stays in the panel.
    assert!(s[2].contains("\"focus\":1"), "{}", s[2]);
    assert!(s[2].contains("x is never used"), "{}", s[2]);
    // Enter: the editor at a.js line 2.
    assert!(s[3].contains("\"focus\":0"), "{}", s[3]);
    assert!(s[3].contains("\"line\":2,\"col\":5"), "{}", s[3]);
    // F4: the next problem, line 3.
    assert!(s[4].contains("\"line\":3,\"col\":1"), "{}", s[4]);
    assert!(s[4].contains("Problem 2/3"), "{}", s[4]);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn tests_view_runs_syncs_and_builds() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("testview");
    let dir = fs::canonicalize(&dir).unwrap();
    let web = dir.join("web");
    let api = dir.join("Api.Tests");
    fs::create_dir_all(web.join("tests")).unwrap();
    fs::create_dir_all(web.join("node_modules/.bin")).unwrap();
    fs::create_dir_all(&api).unwrap();
    fs::write(
        web.join("package.json"),
        r#"{"devDependencies":{"vitest":"^2"}}"#,
    )
    .unwrap();
    let spec = web.join("tests/a.spec.js");
    fs::write(
        &spec,
        "describe('s', () => {\n  it('one', () => {\n    expect(1).toBe(1);\n  });\n  it('two', () => {});\n});",
    )
    .unwrap();
    fs::write(
        api.join("Api.Tests.csproj"),
        "<Project><ItemGroup><PackageReference Include=\"xunit\" /></ItemGroup></Project>",
    )
    .unwrap();
    let cs = api.join("CalcTests.cs");
    fs::write(
        &cs,
        "namespace Api.Tests;\n\npublic class CalcTests\n{\n    [Fact]\n    public void Adds() { Assert.Equal(2, 1 + 1); }\n}\n",
    )
    .unwrap();
    let ws = dir.join("w.jmxws");
    fs::write(
        &ws,
        r#"{"version":1,"name":"w","folders":["web","Api.Tests"],"respect_gitignore":true}"#,
    )
    .unwrap();

    let make = |path: &Path, body: String| {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let report = format!(
        r#"{{"testResults":[{{"name":"{}","assertionResults":[{{"ancestorTitles":["s"],"title":"one","status":"passed","duration":3}},{{"ancestorTitles":["s"],"title":"two","status":"failed","failureMessages":["boom"]}}]}}]}}"#,
        spec.display()
    );
    make(
        &web.join("node_modules/.bin/vitest"),
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in --outputFile=*) out=\"${{a#--outputFile=}}\";; esac; done\nprintf '%s' '{report}' > \"$out\"\nexit 1\n"
        ),
    );
    let trx = r#"<TestRun><Results><UnitTestResult testId="1" testName="Api.Tests.CalcTests.Adds" outcome="Passed" duration="00:00:00.0010000" /></Results><TestDefinitions><UnitTest id="1"><TestMethod className="Api.Tests.CalcTests" name="Adds" /></UnitTest></TestDefinitions></TestRun>"#;
    let warning = format!(
        "{}(6,25): warning CS0219: The variable 'z' is assigned but its value is never used [{}]",
        cs.display(),
        api.join("Api.Tests.csproj").display()
    );
    let dotnet = dir.join("fake-dotnet");
    make(
        &dotnet,
        format!(
            "#!/bin/sh\necho '{warning}'\nif [ \"$1\" = test ]; then\n  for a in \"$@\"; do case \"$a\" in 'trx;LogFileName='*) out=\"${{a#trx;LogFileName=}}\";; esac; done\n  printf '%s' '{trx}' > \"$out\"\nfi\n"
        ),
    );

    let script = "wait\n\
                  state\n\
                  key ctrl+shift+t\n\
                  key shift+f5\n\
                  wait\n\
                  state\n\
                  key ctrl+1\n\
                  key alt+1\n\
                  key ctrl+home\n\
                  key down\n\
                  key end\n\
                  key enter\n\
                  type     x();\n\
                  wait\n\
                  state\n\
                  key ctrl+end\n\
                  key enter\n\
                  type it('three', () => {});\n\
                  wait\n\
                  state\n\
                  key ctrl+tab\n\
                  key ctrl+s\n\
                  wait\n\
                  state\n\
                  quit\n";
    let script_path = dir.join("tv.jmxs");
    fs::write(&script_path, script).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_jmx"))
        .args(["--headless", "--size=900x500"])
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .arg("--script")
        .arg(&script_path)
        .arg(&ws)
        .arg(&spec)
        .arg(&cs)
        .env("JMX_DOTNET", &dotnet)
        .output()
        .expect("failed to start jmx");
    if out.status.code() == Some(EXIT_NO_GPU) {
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = states(&out);
    let tests = |i: usize| {
        let st = &s[i];
        st[st.find("\"tests\":").unwrap()..].to_string()
    };
    // Found: two specs and one C# test.
    assert!(
        tests(0).contains("\"total\":3,\"passed\":0,\"failed\":0,\"not_run\":3"),
        "{}",
        tests(0)
    );
    // Shift+F5: everything ran.
    assert!(
        tests(1).contains("\"passed\":2,\"failed\":1,\"not_run\":0"),
        "{}",
        tests(1)
    );
    // Editing test 'one' turns it back to not run.
    assert!(
        tests(2).contains("\"passed\":1,\"failed\":1,\"not_run\":1"),
        "{}",
        tests(2)
    );
    // A newly typed test appears, not run.
    assert!(
        tests(3).contains("\"total\":4") && tests(3).contains("\"not_run\":2"),
        "{}",
        tests(3)
    );
    // Saving the C# file builds it: the compiler warning is in Problems.
    assert!(
        s[4].contains("\"problems\":{\"errors\":0,\"warnings\":1"),
        "{}",
        s[4]
    );
    fs::remove_dir_all(&dir).unwrap();
}
