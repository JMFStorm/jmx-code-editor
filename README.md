# JDC — Just Do Code Editor

A lightweight, GPU-rendered, keyboard-first code editor for Linux and Windows, written in
Rust (winit + wgpu + glyphon). Dark theme, fast start, and the tools a web / .NET developer
reaches for every day: workspaces, search, linting, tests and code navigation, without an
extension ecosystem to maintain.

## Features

**Editing**
- Multiple cursors (Shift+Alt+. / Shift+Alt+, add the next occurrence, Ctrl+Shift+L all)
- Delete line (Ctrl+K), revert the file to its saved state (Ctrl+R, one undo step)
- Prettier formatting with the project's own config: Ctrl+S then S formats and saves,
  Shift+Alt+F formats only
- Syntax highlighting for JavaScript / TypeScript, Vue, HTML, CSS, JSON, XML, C# and Odin

**Finding things**
- Quick Open (Ctrl+P) over the workspace, with an optional line: `appvue:42`
- Find in Files (Ctrl+Shift+F) with match case, whole word, regex and a file filter
- Workspaces (`.jdcws`) with several folders, an Explorer that follows the active file,
  gitignore-aware, and the last workspace reopened on start

**Checks**
- Problems list: ESLint for the whole project and as you edit, Roslyn (`dotnet build`)
  errors and warnings, filters for errors / warnings / messages, F4 to step through them
- Tests view (Ctrl+Shift+T): Vitest, Jest, `dotnet test` (xUnit, NUnit, MSTest) and
  `odin test` (`@(test)` procs; extra flags in a project's `jdc.json`, see Help); tests are
  found statically and stay in sync with your edits (a changed test goes back to "not run"),
  failures in a box under the selected test and on the gutter mark

**Code navigation**
- Doc popups (mouse rest or Ctrl+I) and parameter hints while typing a call, from JSDoc and
  C# `///` comments
- Go to Definition (F12, Ctrl+click), Go Back (Alt+Left); on a declaration F12 moves between
  an interface member and its implementations
- Find All References (Shift+F12), listed in the panel; F4 steps through them, and the shown
  place is outlined in the editor
- Framework docs offline: .NET APIs from the installed SDK's and NuGet packages' XML docs
  (namespaces from your `using`s), npm packages from their own `.d.ts` files in
  `node_modules`, JSDoc types followed through `@typedef` to their interfaces
- On request (Ctrl+F12, always asks first): the Microsoft Learn, MDN or package docs page in
  your browser
- Odin through its language server, [ols](https://github.com/DanielGavin/ols): doc popups,
  parameter hints, Go to Definition, references, formatting and compiler problems; Ctrl+F12
  opens pkg.odin-lang.org for Odin's own packages

**Panel**
The right view is for the editor's own pages: Help, Shortcuts, Problems, Tests, Docs and
References. Every page word-wraps to the panel's width; long texts (failures, messages,
summaries) sit in boxes.

## Keys

| Key | Action |
| --- | --- |
| Ctrl+P | Quick Open (`name:line`) |
| Ctrl+Shift+F | Find in Files |
| Ctrl+S, S | Format and save |
| Ctrl+I | Docs / problem / failure at the cursor |
| F12 / Shift+F12 | Go to Definition / Find All References |
| Alt+Left | Go Back |
| Ctrl+F12 | Open the web docs (asks first) |
| F4 / Shift+F4 | Next / previous problem or reference |
| Ctrl+Shift+M | Lint the workspace |
| Ctrl+Shift+T, F5 | Tests view, run the test at the cursor |
| F6, Shift+F6 | Switch between editor and panel, hide the panel |
| F1 / F2 | Help / every shortcut |

## Building

Requirements: Rust (edition 2024) and a GPU with Vulkan, Metal or DirectX 12 (a CPU fallback
renderer exists for headless runs).

```sh
cargo build --release          # target/release/jdc
./build-all.sh                 # dist/jdc-<version>-linux-x86_64 and …-windows-x86_64.exe
```

`build-all.sh` cross-compiles the Windows build with mingw-w64
(`sudo apt install mingw-w64`, `rustup target add x86_64-pc-windows-gnu`).

## Running

```sh
jdc                            # reopens the last workspace
jdc src/main.rs README.md      # open files
jdc my-project.jdcws           # open a workspace
```

State (last workspace, window) and logs live in `~/.local/state/jdc` on Linux and
`%LOCALAPPDATA%\jdc` on Windows. `jdc --help` lists the options.

JDC uses these tools when they are there, and works without them:

- **Node projects:** the project's own `prettier`, `eslint`, `vitest` or `jest` from
  `node_modules/.bin`.
- **.NET:** the `dotnet` SDK, for builds, tests and the offline API docs.
- **Odin:** `odin` (tests) and `ols` (code intelligence, formatting, problems). Build ols with
  the same Odin release you use: newer ols commits can need a newer Odin.
- **Overrides:** `JDC_PRETTIER`, `JDC_ESLINT`, `JDC_VITEST`, `JDC_JEST`, `JDC_DOTNET`, `JDC_ODIN`
  and `JDC_OLS` point to a specific tool; `JDC_LOG` sets the log level.

## Headless mode

For scripted checks and screenshots without a window:

```sh
jdc --headless --size=1500x800 --script check.jdcs my-project.jdcws
```

A script has one command per line: `key ctrl+shift+f`, `type text`, `click X,Y`,
`wheel DY`, `resize WxH`, `wait` (for background work), `state` (prints the editor state as
JSON), `shot file.png`, `quit`.

## Project layout

```
src/
  editor/        the editor model: documents, commands, panel pages, navigation
  symbols/       code indexing: JS/TS/Vue, C#, .d.ts, .NET XML docs, npm packages, MDN
  syntax/        highlighting lexers
  lint/          ESLint and Roslyn
  lsp/           language server client (ols for Odin)
  test_explorer/ test discovery and runners
  workspace/     workspaces, file index, search
  render/        wgpu renderer
  ui/            explorer, picker, menus, layout
sandbox/         mock projects for trying it out (Vue dashboard, .NET inventory API, Odin)
tests/           headless end-to-end tests
```
