// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod commands;
mod config;
mod editor;
mod help;
mod keymap;
mod render;
mod theme;
mod ui;

use std::path::PathBuf;

use winit::event_loop::EventLoop;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut paths = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!(
                    "jmx {} - lightweight GPU text editor\n",
                    env!("CARGO_PKG_VERSION")
                );
                println!("Usage: jmx [FILE]...\n\n{}", keymap::listing_text());
                return Ok(());
            }
            Some("-V" | "--version") => {
                println!("jmx {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }

    let editor = editor::Editor::new(config::Config::default(), &paths);
    let event_loop = EventLoop::new()?;
    let mut app = app::App::new(editor);
    event_loop.run_app(&mut app)?;
    Ok(())
}
