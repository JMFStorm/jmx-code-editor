//! Headless mode: no window, offscreen rendering, input from a script.
//! Nothing touches the desktop, so it can run while the machine is used normally.
//!
//! Script commands (one per line, `#` comments):
//!   key ctrl+shift+k     press a key combination
//!   type some text       type text, one key event per character
//!   move X,Y             move the mouse (physical pixels)
//!   click X,Y            move + press + release the left button
//!   dclick X,Y           double-click
//!   press X,Y / release X,Y
//!   wheel DY [X,Y]       scroll DY lines (positive = up, like a wheel notch)
//!   resize WxH           resize the render target
//!   shot PATH.png        render a frame and save it as PNG
//!   state                print the editor state as one JSON line
//!   sleep MS             wait (e.g. for status messages to expire)
//!   quit                 same as Ctrl+Q

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest a `state`, `shot` or `wait` waits for background work.
const JOB_TIMEOUT: Duration = Duration::from_secs(10);

use anyhow::{Context, Result, anyhow, bail};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::editor::Editor;
use crate::editor::movement as mv;
use crate::render::{Renderer, frame};
use crate::session::Session;

pub struct HeadlessOptions {
    pub script: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub software: bool,
}

pub const EXIT_SCRIPT_ERROR: i32 = 2;
pub const EXIT_NO_GPU: i32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum ScriptCmd {
    Key(ModifiersState, Key, Option<String>),
    Type(String),
    Move(f32, f32),
    Click(f32, f32),
    DoubleClick(f32, f32),
    Press(f32, f32),
    Release(f32, f32),
    Wheel(f32, Option<(f32, f32)>),
    Resize(u32, u32),
    Shot(PathBuf),
    /// Screenshot without waiting for background work (to see progress indicators).
    ShotNow(PathBuf),
    State,
    /// Wait for background work (Find in Files) to finish.
    Wait,
    Sleep(u64),
    Quit,
}

/// Run a headless session; returns the process exit code.
pub fn run(editor: Editor, opts: HeadlessOptions) -> i32 {
    let fonts = editor.config.font_families.clone();
    let mut renderer = match pollster::block_on(Renderer::new_offscreen(
        opts.width,
        opts.height,
        opts.software,
        &fonts,
    )) {
        Ok(r) => r,
        Err(e) => {
            log::error!("headless GPU initialization failed: {e:#}");
            eprintln!("jdc: headless GPU initialization failed: {e:#}");
            return EXIT_NO_GPU;
        }
    };
    let mut session = Session::new(editor, opts.width, opts.height, opts.scale);
    // Headless runs never start a browser; the URL shows in the state instead.
    session.editor.launch_urls = false;
    session.relayout(&mut renderer);

    let reader: Box<dyn BufRead> = match &opts.script {
        Some(path) => match File::open(path) {
            Ok(f) => Box::new(BufReader::new(f)),
            Err(e) => {
                eprintln!("jdc: cannot open script {}: {e}", path.display());
                return EXIT_SCRIPT_ERROR;
            }
        },
        None => Box::new(io::stdin().lock()),
    };
    log::info!(
        "headless session {}x{} scale {}",
        opts.width,
        opts.height,
        opts.scale
    );

    for (n, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("jdc: error reading script: {e}");
                return EXIT_SCRIPT_ERROR;
            }
        };
        let result = parse_line(&line).and_then(|cmd| match cmd {
            Some(cmd) => {
                log::debug!("script line {}: {cmd:?}", n + 1);
                execute(&mut session, &mut renderer, cmd)
            }
            None => Ok(()),
        });
        if let Err(e) = result {
            eprintln!("error line {}: {e:#}", n + 1);
            log::error!("script error line {}: {e:#}", n + 1);
            return EXIT_SCRIPT_ERROR;
        }
        if session.should_quit() {
            break;
        }
    }
    0
}

fn execute(session: &mut Session, renderer: &mut Renderer, cmd: ScriptCmd) -> Result<()> {
    let wait_first = matches!(cmd, ScriptCmd::Shot(_));
    match cmd {
        ScriptCmd::Key(mods, key, text) => {
            session.modifiers = mods;
            session.key(&key, &key, text.as_deref());
            session.modifiers = ModifiersState::empty();
        }
        ScriptCmd::Type(text) => {
            for ch in text.chars() {
                let s = ch.to_string();
                let key = Key::Character(s.as_str().into());
                session.key(&key, &key, Some(&s));
            }
        }
        ScriptCmd::Move(x, y) => {
            session.mouse_move(x, y);
        }
        ScriptCmd::Click(x, y) => {
            session.mouse_move(x, y);
            session.mouse_press();
            session.mouse_release();
        }
        ScriptCmd::DoubleClick(x, y) => {
            session.mouse_move(x, y);
            for _ in 0..2 {
                session.mouse_press();
                session.mouse_release();
                // Layout can change between clicks (picker entering a folder).
                session.relayout(renderer);
            }
        }
        ScriptCmd::Press(x, y) => {
            session.mouse_move(x, y);
            session.mouse_press();
        }
        ScriptCmd::Release(x, y) => {
            session.mouse_move(x, y);
            session.mouse_release();
        }
        ScriptCmd::Wheel(dy, at) => {
            if let Some((x, y)) = at {
                session.mouse_move(x, y);
            }
            session.wheel_lines(0.0, dy);
        }
        ScriptCmd::Resize(w, h) => {
            renderer.resize(w, h);
            let scale = session.layout.as_ref().map_or(1.0, |l| l.scale);
            session.set_size(w, h, scale);
        }
        ScriptCmd::Shot(path) | ScriptCmd::ShotNow(path) => {
            if wait_first {
                session.editor.wait_jobs(JOB_TIMEOUT);
            } else {
                session.editor.poll_jobs();
            }
            if !session.draw(renderer, true) {
                bail!("frame was not rendered");
            }
            let (w, h, pixels) = renderer.gpu.capture()?;
            write_png(&path, w, h, &pixels)?;
            println!("shot {}", path.display());
        }
        ScriptCmd::State => {
            session.editor.wait_jobs(JOB_TIMEOUT);
            println!("{}", state_json(session));
        }
        ScriptCmd::Wait => session.editor.wait_jobs(JOB_TIMEOUT),
        ScriptCmd::Sleep(ms) => {
            std::thread::sleep(Duration::from_millis(ms));
            session.editor.expire_message();
            session.tick();
        }
        ScriptCmd::Quit => session.editor.execute(crate::commands::Command::Quit),
    }
    io::stdout().flush().ok();
    session.relayout(renderer);
    Ok(())
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let file = File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

// ---------------------------------------------------------------- parsing

fn parse_xy(s: &str) -> Result<(f32, f32)> {
    let (x, y) = s
        .split_once(',')
        .ok_or_else(|| anyhow!("expected X,Y but got {s:?}"))?;
    Ok((x.trim().parse()?, y.trim().parse()?))
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| anyhow!("expected WxH but got {s:?}"))?;
    let (w, h) = (w.trim().parse()?, h.trim().parse()?);
    if w == 0 || h == 0 {
        bail!("size must be non-zero");
    }
    Ok((w, h))
}

pub fn parse_line(line: &str) -> Result<Option<ScriptCmd>> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let arg = rest.trim();
    let need = |what: &str| -> Result<()> {
        if arg.is_empty() {
            bail!("`{cmd}` needs {what}");
        }
        Ok(())
    };
    Ok(Some(match cmd {
        "key" => {
            need("a key, e.g. ctrl+s")?;
            let (mods, key, text) = parse_key(arg)?;
            ScriptCmd::Key(mods, key, text)
        }
        // Keep the text exactly as written after the single separating space.
        "type" => ScriptCmd::Type(rest.to_string()),
        "move" => {
            let (x, y) = parse_xy(arg)?;
            ScriptCmd::Move(x, y)
        }
        "click" => {
            let (x, y) = parse_xy(arg)?;
            ScriptCmd::Click(x, y)
        }
        "dclick" => {
            let (x, y) = parse_xy(arg)?;
            ScriptCmd::DoubleClick(x, y)
        }
        "press" => {
            let (x, y) = parse_xy(arg)?;
            ScriptCmd::Press(x, y)
        }
        "release" => {
            let (x, y) = parse_xy(arg)?;
            ScriptCmd::Release(x, y)
        }
        "wheel" => {
            need("a line count")?;
            let mut parts = arg.split_whitespace();
            let dy = parts.next().unwrap_or_default().parse()?;
            let at = parts.next().map(parse_xy).transpose()?;
            ScriptCmd::Wheel(dy, at)
        }
        "resize" => {
            let (w, h) = parse_size(arg)?;
            ScriptCmd::Resize(w, h)
        }
        "shot" => {
            need("an output path")?;
            ScriptCmd::Shot(PathBuf::from(arg))
        }
        "shotnow" => {
            need("an output path")?;
            ScriptCmd::ShotNow(PathBuf::from(arg))
        }
        "state" => ScriptCmd::State,
        "wait" => ScriptCmd::Wait,
        "sleep" => {
            need("milliseconds")?;
            ScriptCmd::Sleep(arg.parse()?)
        }
        "quit" => ScriptCmd::Quit,
        other => bail!("unknown command `{other}`"),
    }))
}

/// `ctrl+shift+k` → modifiers, key, and the text a real key press would produce.
pub fn parse_key(spec: &str) -> Result<(ModifiersState, Key, Option<String>)> {
    let lower = spec.trim().to_lowercase();
    // A trailing "+" key ("ctrl++") would otherwise split into empty parts.
    let (mods_part, key_part) = match lower.strip_suffix("++") {
        Some(m) => (m, "+"),
        None => lower.rsplit_once('+').unwrap_or(("", lower.as_str())),
    };
    let mut mods = ModifiersState::empty();
    for m in mods_part.split('+').filter(|m| !m.is_empty()) {
        mods |= match m {
            "ctrl" | "control" => ModifiersState::CONTROL,
            "shift" => ModifiersState::SHIFT,
            "alt" | "option" => ModifiersState::ALT,
            "super" | "cmd" | "meta" | "win" => ModifiersState::SUPER,
            other => bail!("unknown modifier `{other}`"),
        };
    }
    let named = match key_part {
        "enter" | "return" => Some(NamedKey::Enter),
        "esc" | "escape" => Some(NamedKey::Escape),
        "tab" => Some(NamedKey::Tab),
        "backspace" => Some(NamedKey::Backspace),
        "delete" | "del" => Some(NamedKey::Delete),
        "insert" | "ins" => Some(NamedKey::Insert),
        "home" => Some(NamedKey::Home),
        "end" => Some(NamedKey::End),
        "pageup" | "pgup" => Some(NamedKey::PageUp),
        "pagedown" | "pgdn" => Some(NamedKey::PageDown),
        "up" => Some(NamedKey::ArrowUp),
        "down" => Some(NamedKey::ArrowDown),
        "left" => Some(NamedKey::ArrowLeft),
        "right" => Some(NamedKey::ArrowRight),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            match f[1..].parse::<u8>().unwrap_or(0) {
                1 => Some(NamedKey::F1),
                2 => Some(NamedKey::F2),
                3 => Some(NamedKey::F3),
                4 => Some(NamedKey::F4),
                5 => Some(NamedKey::F5),
                6 => Some(NamedKey::F6),
                7 => Some(NamedKey::F7),
                8 => Some(NamedKey::F8),
                9 => Some(NamedKey::F9),
                10 => Some(NamedKey::F10),
                11 => Some(NamedKey::F11),
                12 => Some(NamedKey::F12),
                n => bail!("unsupported function key F{n}"),
            }
        }
        _ => None,
    };
    if let Some(k) = named {
        let text = match k {
            NamedKey::Enter => Some("\r".to_string()),
            NamedKey::Tab => Some("\t".to_string()),
            _ => None,
        };
        return Ok((mods, Key::Named(k), text));
    }
    let ch = match key_part {
        "space" => ' ',
        "plus" => '+',
        "minus" => '-',
        s if s.chars().count() == 1 => s.chars().next().unwrap_or(' '),
        other => bail!("unknown key `{other}`"),
    };
    // Like a real keyboard: shift gives uppercase letters.
    let ch = if mods.shift_key() {
        ch.to_ascii_uppercase()
    } else {
        ch
    };
    let s = ch.to_string();
    Ok((mods, Key::Character(s.as_str().into()), Some(s)))
}

// ---------------------------------------------------------------- state output

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One-line JSON snapshot of the editor, for scripted assertions.
pub fn state_json(session: &Session) -> String {
    let ed = &session.editor;
    let opt = |v: Option<String>| v.unwrap_or_else(|| "null".to_string());
    let tabs_of = |pane: usize| -> String {
        ed.panes[pane]
            .docs
            .iter()
            .map(|d| {
                format!(
                    "{{\"title\":{},\"label\":{},\"dirty\":{}}}",
                    json_str(&d.title()),
                    json_str(&crate::editor::hover::tab_title(ed, d).text),
                    d.is_dirty()
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let panes: Vec<String> = (0..ed.panes.len())
        .map(|p| {
            let doc = ed.panes[p].doc();
            format!(
                "{{\"tabs\":[{}],\"active\":{},\"top_line\":{},\"cursor_line\":{}}}",
                tabs_of(p),
                ed.panes[p].active,
                doc.map_or(0, |d| d.view.top_line),
                doc.map_or(0, |d| d.rope.char_to_line(d.selection.head) + 1)
            )
        })
        .collect();

    // Fields about the focused document; null when the focused view is empty.
    let doc = ed.pane().doc();
    let (mut path, mut dirty, mut read_only, mut line, mut col) = (None, None, None, None, None);
    let (mut selection, mut line_text, mut lines, mut indent, mut line_ending) =
        (None, None, None, None, None);
    let (mut cursors, mut lang) = (None, None);
    if let Some(doc) = doc {
        let head = doc.selection.head;
        let l = doc.rope.char_to_line(head);
        let (s, e) = doc.selection.range();
        path = doc
            .path
            .as_ref()
            .map(|p| json_str(&p.display().to_string()));
        dirty = Some(doc.is_dirty().to_string());
        read_only = Some(doc.read_only.to_string());
        line = Some((l + 1).to_string());
        col = Some((mv::col_of(&doc.rope, head, ed.config.tab_width) + 1).to_string());
        selection = Some(format!("[{s},{e}]"));
        cursors = Some((doc.extra.len() + 1).to_string());
        lang = Some(json_str(doc.syntax.lang.name()));
        let text: String = doc.rope.line(l).chars().filter(|&c| c != '\n').collect();
        line_text = Some(json_str(&text));
        lines = Some(doc.rope.len_lines().to_string());
        indent = Some(json_str(&doc.indent.label()));
        line_ending = Some(json_str(doc.line_ending.label()));
    }
    let picker = ed.picker.as_ref().map(|p| {
        format!(
            "{{\"kind\":{},\"dir\":{},\"filter\":{},\"selected\":{},\"count\":{}}}",
            json_str(&format!("{:?}", p.kind)),
            json_str(&p.dir.display().to_string()),
            json_str(&p.filter),
            opt(p.selected_entry().map(|e| json_str(&e.name))),
            p.len()
        )
    });
    let workspace = ed.workspace.as_ref().map(|w| {
        let folders: Vec<String> = w
            .folders
            .iter()
            .map(|f| json_str(&f.display().to_string()))
            .collect();
        format!(
            "{{\"name\":{},\"folders\":[{}],\"file\":{},\"gitignore\":{}}}",
            json_str(&w.name),
            folders.join(","),
            opt(w.file.as_ref().map(|f| json_str(&f.display().to_string()))),
            w.respect_gitignore
        )
    });
    let ex = &ed.explorer;
    let rows: Vec<String> = ex
        .rows
        .iter()
        .map(|r| json_str(&format!("{}{}", "  ".repeat(r.depth), r.name)))
        .collect();
    let explorer = format!(
        "{{\"visible\":{},\"focused\":{},\"selected\":{},\"rows\":[{}]}}",
        ex.visible,
        ex.focused,
        ex.selected,
        rows.join(",")
    );
    let f = &ed.find;
    let o = ed.search_opts;
    let find = format!(
        "{{\"open\":{},\"query\":{},\"include\":{},\"case\":{},\"word\":{},\"regex\":{},\
\"files\":{},\"matches\":{},\"running\":{},\"error\":{},\"selected\":{}}}",
        f.open,
        json_str(&f.query),
        json_str(&f.include),
        o.case_sensitive,
        o.whole_word,
        o.regex,
        f.results.len(),
        f.match_count,
        f.running(),
        opt(f.error.as_ref().map(|e| json_str(e))),
        opt(f.selected_hit().map(|(file, hit)| json_str(&format!(
            "{}:{}",
            file.rel,
            hit.line + 1
        )))),
    );
    let (errors, warnings) = ed.problems.counts();
    let problems = format!(
        "{{\"errors\":{errors},\"warnings\":{warnings},\"files\":{},\"linting\":{}}}",
        ed.problems.file_count(),
        !ed.lint_jobs.is_empty()
    );
    let (passed, failed, not_run, running) = ed.tests.tree.counts();
    let tests = format!(
        "{{\"total\":{},\"passed\":{passed},\"failed\":{failed},\"not_run\":{not_run},\"running\":{running}}}",
        ed.tests.tree.tests.len()
    );
    let popup = |h: &crate::editor::hover::Hover| {
        json_str(
            &h.lines
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    let hover = opt(ed.hover.as_ref().map(popup));
    let signature = opt(ed.symbols.signature.as_ref().map(popup));
    let opened_url = opt(ed.opened_url.as_deref().map(json_str));
    let located = opt(ed.panes[crate::editor::EDITOR].doc().and_then(|d| {
        let l = ed.located_in(d)?;
        let name = l.path.file_name()?.to_string_lossy().into_owned();
        Some(json_str(&format!(
            "{name}:{}:{}-{}",
            l.line + 1,
            l.start_col + 1,
            l.end_col + 1
        )))
    }));
    let references = format!(
        "{{\"count\":{},\"files\":{},\"running\":{}}}",
        ed.refs.count(),
        ed.refs.results.len(),
        ed.refs.running()
    );
    let menu = session.open_menu_id().map(|m| json_str(&format!("{m:?}")));
    let prompt = ed.minibuffer.as_ref().map(|m| json_str(&m.input));
    let message = ed.message.as_ref().map(|m| json_str(&m.text));
    format!(
        "{{\"mode\":{},\"focus\":{},\"split\":{},\"split_ratio\":{:.3},\"menu\":{},\
\"active\":{},\"tabs\":[{}],\"panes\":[{}],\"path\":{},\"dirty\":{},\"read_only\":{},\
\"line\":{},\"col\":{},\"selection\":{},\"cursors\":{},\"lang\":{},\"line_text\":{},\"lines\":{},\"indent\":{},\
\"line_ending\":{},\"picker\":{},\"prompt\":{},\"message\":{},\"font_size\":{},\
\"workspace\":{},\"explorer\":{explorer},\"find\":{find},\"formatting\":{},\"problems\":{problems},\"tests\":{tests},\"hover\":{hover},\"signature\":{signature},\"opened_url\":{opened_url},\"references\":{references},\"located\":{located}}}",
        json_str(frame::mode_label(ed)),
        ed.focus,
        ed.split,
        ed.split_ratio,
        opt(menu),
        ed.pane().active,
        tabs_of(ed.focus),
        panes.join(","),
        opt(path),
        opt(dirty),
        opt(read_only),
        opt(line),
        opt(col),
        opt(selection),
        opt(cursors),
        opt(lang),
        opt(line_text),
        opt(lines),
        opt(indent),
        opt(line_ending),
        opt(picker),
        opt(prompt),
        opt(message),
        ed.config.font_size,
        opt(workspace),
        !ed.format_jobs.is_empty(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_specs() {
        let (m, k, t) = parse_key("ctrl+shift+k").unwrap();
        assert_eq!(m, ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert_eq!(k, Key::Character("K".into()));
        assert_eq!(t.as_deref(), Some("K"));

        let (m, k, _) = parse_key("Enter").unwrap();
        assert!(m.is_empty());
        assert_eq!(k, Key::Named(NamedKey::Enter));

        let (m, k, _) = parse_key("alt+1").unwrap();
        assert_eq!(m, ModifiersState::ALT);
        assert_eq!(k, Key::Character("1".into()));

        let (_, k, _) = parse_key("shift+f1").unwrap();
        assert_eq!(k, Key::Named(NamedKey::F1));
        let (m, k, _) = parse_key("ctrl++").unwrap();
        assert_eq!(m, ModifiersState::CONTROL);
        assert_eq!(k, Key::Character("+".into()));
        assert!(parse_key("hyper+x").is_err());
        assert!(parse_key("f99").is_err());
    }

    #[test]
    fn script_lines() {
        assert_eq!(parse_line("  # comment").unwrap(), None);
        assert_eq!(parse_line("").unwrap(), None);
        assert_eq!(
            parse_line("type  two spaces").unwrap(),
            Some(ScriptCmd::Type(" two spaces".into()))
        );
        assert_eq!(
            parse_line("click 75,15").unwrap(),
            Some(ScriptCmd::Click(75.0, 15.0))
        );
        assert_eq!(
            parse_line("wheel -3 10,20").unwrap(),
            Some(ScriptCmd::Wheel(-3.0, Some((10.0, 20.0))))
        );
        assert_eq!(
            parse_line("resize 800x600").unwrap(),
            Some(ScriptCmd::Resize(800, 600))
        );
        assert_eq!(
            parse_line("shot out/a.png").unwrap(),
            Some(ScriptCmd::Shot("out/a.png".into()))
        );
        assert!(parse_line("click 75").is_err());
        assert!(parse_line("frobnicate").is_err());
        assert!(parse_line("shot").is_err());
    }

    #[test]
    fn json_escaping() {
        assert_eq!(json_str("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
    }
}
