// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod app_state;
mod commands;
mod config;
mod editor;
mod format;
mod headless;
mod help;
mod keymap;
mod lint;
mod logging;
mod paths;
mod process;
mod render;
mod session;
mod syntax;
mod test_explorer;
mod theme;
mod ui;
mod workspace;

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use winit::event_loop::EventLoop;

pub const USAGE: &str = "\
Usage: jmx [OPTIONS] [FILE]...

Options:
  --headless          run without a window; read script commands (see below)
  --script FILE       headless script to run (default: read commands from stdin)
  --size WxH          headless render size in pixels (default 1100x720)
  --scale F           headless display scale factor (default 1)
  --software          headless: use the CPU fallback renderer
  --no-restore        do not reopen the last workspace
  --log-dir DIR       folder for log files
  --log-level SPEC    log filter: trace | debug,wgpu_hal=info | *=info (env: JMX_LOG)
  -h, --help          show this help
  -V, --version       show the version

Headless script commands:
  key ctrl+shift+k | type TEXT | move X,Y | click X,Y | dclick X,Y
  press X,Y | release X,Y | wheel DY [X,Y] | resize WxH | shot FILE.png
  state | wait | sleep MS | quit";

struct Args {
    paths: Vec<PathBuf>,
    headless: bool,
    script: Option<PathBuf>,
    size: (u32, u32),
    scale: f32,
    software: bool,
    log_dir: Option<PathBuf>,
    log_level: Option<String>,
    no_restore: bool,
    /// State file override (tests); headless runs use none unless given.
    state_file: Option<PathBuf>,
}

enum Parsed {
    Run(Args),
    Exit,
}

fn parse_args(raw: impl Iterator<Item = OsString>) -> Result<Parsed> {
    let mut args = Args {
        paths: Vec::new(),
        headless: false,
        script: None,
        size: (1100, 720),
        scale: 1.0,
        software: false,
        log_dir: None,
        log_level: None,
        no_restore: false,
        state_file: None,
    };
    let mut raw = raw.peekable();
    let mut only_files = false;
    while let Some(arg) = raw.next() {
        let Some(s) = arg.to_str().filter(|s| !only_files && s.starts_with('-')) else {
            args.paths.push(PathBuf::from(arg));
            continue;
        };
        let (name, inline) = match s.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (s.to_string(), None),
        };
        let mut value = |what: &str| -> Result<String> {
            if let Some(v) = &inline {
                return Ok(v.clone());
            }
            raw.next()
                .and_then(|v| v.into_string().ok())
                .ok_or_else(|| anyhow!("{name} needs {what}"))
        };
        match name.as_str() {
            "-h" | "--help" => {
                println!(
                    "jmx {} - lightweight GPU text editor\n",
                    env!("CARGO_PKG_VERSION")
                );
                println!("{USAGE}\n");
                match logging::default_dir() {
                    Some(d) => println!("Logs: {}\n", d.display()),
                    None => println!("Logs: (no log folder found)\n"),
                }
                println!("{}", keymap::listing_text());
                return Ok(Parsed::Exit);
            }
            "-V" | "--version" => {
                println!("jmx {}", env!("CARGO_PKG_VERSION"));
                return Ok(Parsed::Exit);
            }
            "--headless" => args.headless = true,
            "--software" => args.software = true,
            "--no-restore" => args.no_restore = true,
            "--state-file" => args.state_file = Some(PathBuf::from(value("a file")?)),
            "--script" => args.script = Some(PathBuf::from(value("a file")?)),
            "--size" => {
                let v = value("WxH")?;
                let (w, h) = v
                    .split_once(['x', 'X'])
                    .ok_or_else(|| anyhow!("--size expects WxH, got {v}"))?;
                args.size = (
                    w.parse().context("bad --size width")?,
                    h.parse().context("bad --size height")?,
                );
            }
            "--scale" => {
                args.scale = value("a number")?.parse().context("bad --scale")?;
                if !(0.25..=8.0).contains(&args.scale) {
                    bail!("--scale must be between 0.25 and 8");
                }
            }
            "--log-dir" => args.log_dir = Some(PathBuf::from(value("a folder")?)),
            "--log-level" => args.log_level = Some(value("a filter")?),
            "--" => only_files = true,
            other => bail!("unknown option {other} (see --help)"),
        }
    }
    if args.script.is_some() && !args.headless {
        bail!("--script requires --headless");
    }
    Ok(Parsed::Run(args))
}

fn main() {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Exit) => return,
        Err(e) => {
            eprintln!("jmx: {e:#}");
            std::process::exit(headless::EXIT_SCRIPT_ERROR);
        }
    };

    logging::init(logging::LogOptions {
        dir: args.log_dir.clone(),
        name: if args.headless { "jmx-headless" } else { "jmx" },
        spec: args.log_level.clone(),
    });
    log::info!(
        "starting{}, files: {:?}, log: {}",
        if args.headless { " headless" } else { "" },
        args.paths,
        logging::log_path().map_or("(stderr)".into(), |p| p.display().to_string())
    );
    if cfg!(debug_assertions) && std::env::var_os("JMX_TEST_PANIC").is_some() {
        panic!("JMX_TEST_PANIC is set: testing the panic hook");
    }

    let code = match run(args) {
        Ok(code) => code,
        Err(e) => {
            log::error!("{e:#}");
            eprintln!("jmx: {e:#}");
            1
        }
    };
    log::info!("exit with code {code}");
    logging::flush();
    std::process::exit(code);
}

fn run(args: Args) -> Result<i32> {
    let mut editor = editor::Editor::new(config::Config::default(), &args.paths);
    // Remember (and with no files given, reopen) the last workspace. Headless runs leave the
    // user's state alone unless a state file is given.
    let state_file = match (&args.state_file, args.headless) {
        (Some(f), _) => Some(f.clone()),
        (None, false) => paths::state_file(),
        (None, true) => None,
    };
    if let Some(file) = state_file {
        editor.attach_state(file, args.paths.is_empty() && !args.no_restore);
    }
    if args.headless {
        return Ok(headless::run(
            editor,
            headless::HeadlessOptions {
                script: args.script,
                width: args.size.0,
                height: args.size.1,
                scale: args.scale,
                software: args.software,
            },
        ));
    }
    let event_loop = EventLoop::new()?;
    let mut app = app::App::new(editor);
    event_loop.run_app(&mut app)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(list: &[&str]) -> Result<Parsed> {
        parse_args(list.iter().map(OsString::from))
    }

    #[test]
    fn options() {
        let Parsed::Run(a) = parse(&[
            "--headless",
            "--size=800x600",
            "--script",
            "s.txt",
            "a.txt",
            "--",
            "--b",
        ])
        .unwrap() else {
            panic!("expected run");
        };
        assert!(a.headless);
        assert_eq!(a.size, (800, 600));
        assert_eq!(a.script, Some(PathBuf::from("s.txt")));
        assert_eq!(a.paths, [PathBuf::from("a.txt"), PathBuf::from("--b")]);
        assert!(parse(&["--script", "x"]).is_err()); // needs --headless
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--size", "big"]).is_err());
    }
}
