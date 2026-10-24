# Performance and memory: stress suite and results

## The stress suite

```sh
tools/make-stress-sandbox.py     # sandbox/stress (deterministic; about 27k files, 91 MB)
tools/stress/run.sh LABEL        # every scenario, headless, release build
tools/stress/run.sh LABEL s04    # one scenario
```

The results go to `target/stress/LABEL.jsonl`. The run prints them side by side with
`target/stress/baseline.jsonl` when that file exists.

**The sandbox**
- **JS/TS/Vue monorepo:**
  - about 20k source files in 40 packages, with JSDoc and typedefs
  - 2k spec files (about 20k tests)
  - a fake `node_modules` (3k folders) and `dist/` build output
- **Big files:**
  - `big.js`: 200k lines, 8 MB
  - `bundle.min.js`: 3 MB on one line
  - `data.json`: 20 MB
  - `server.log`: every line matches
- **.NET:** 4 C# projects (1,600 files) and a test project with 2k tests.

**How it measures**
- **Headless script commands:**
  - `perf LABEL`: time since the last mark, RSS, peak RSS, heap in use, and work counters (frames, lines lexed, page rebuilds, symbol scans, processes)
  - `frames N`: time per frame
  - `typef TEXT` and `keys KEY N`: time per keystroke, including the frame it causes
- **C# builds** are an external tool's time. The runner makes them fail at once (`JDC_DOTNET=/bin/false`), while the SDK's API docs still load.

**Scenarios**
1. **`s01_open`:** open the workspace and wait for every scan.
2. **`s02_quickopen`:** Quick Open, typing a name.
3. **`s03_find`:** Find in Files for a common word.
4. **`s04_bigfile`:** `big.js`:
   - Ctrl+End
   - an edit at the top, then the end
   - paging
   - Select All Occurrences (25k cursors) and typing
5. **`s05_minified`:** `bundle.min.js`: scrolling far right and typing.
6. **`s06_hints`:** typing a call with parameter hints in `big.js`.
7. **`s07_pages`:** the Tests (20k tests) and Problems pages, with arrow keys.
8. **`s08_refs`:** Find All References on a widely used function.
9. **`s09_outside`:** 2,000 files changed on disk while open.
10. **`s10_session`:** a longer editing session with undo and redo.

## Results (before → after)

All figures are from the same machine.

| Scenario | Before | After |
| --- | --- | --- |
| Open the stress workspace, until every scan is done | ~31 s | **0.4 s** |
| Memory after opening (RSS; GPU driver and fonts are ~110 MB of it) | 365 MB | **259 MB** |
| JDC's own heap after opening | not measured | 131 MB |
| Typing with parameter hints in an 8 MB file (per key) | **~20 s** | **6 ms** |
| 25k cursors: per frame / typing a key | 90 ms / 100 ms | **1.7 ms / 13 ms** |
| Ctrl+End in a 200k-line file, worst frame | 110 ms | **11 ms** |
| Edit at the top, then the end, worst frame | 100 ms | **4.6 ms** |
| End of a 3 MB minified line: key / frame | 30 ms / 23 ms | **6 ms / 4.7 ms** |
| Moving right through a minified line (per key) | 3.4 ms | 1.0 ms |
| Opening the Tests page (20k tests) | 904 ms | **139 ms** |
| Find in Files, Quick Open, references | 225 / 27 / 100 ms | about the same |

Arrow keys on the Tests and Problems pages cost 1.4–2.4 ms per key, against 0.3–0.9 ms before. Each step now previews the test's file in the editor and keeps its highlight. Find in Files, Quick Open and Find All References were already fast; they now use less memory.

## What changed

**Opening a workspace**
- **The test scan is merged in one pass with one sort,** not a full re-sort per file (about 30 s of UI-thread time).
- **The symbol index maps names to `u32` file ids** with names deduplicated per file. The old index had an O(n²) insert and a `PathBuf` copy per name.
- **The symbol scan runs on every core** (1.3 s → 0.2 s).
- **The workspace file's path is made absolute,** so its folders are too. A bug fix: with a relative path, `dotnet build` ran in the wrong folder and never found the project.
- **The file watcher watches only the folders the gitignore-aware walker keeps** (176 instead of about 3.2k: no `node_modules`). Its folders are added on a background thread, and duplicate events are removed with a map.

**Typing and drawing**
- **Line and column lookups in the scanners** use a line-start table, not a newline count from the file's start for every symbol. That count was quadratic and caused the 20 s keystrokes.
- **Big open documents are rescanned for symbols in the background,** 300 ms after typing stops. Small ones are still rescanned right away.
- **C# lookups read only the head of the file** for its `using`s. They used to copy the whole file twice per hover.
- **Highlighting:**
  - After an edit, only the edited lines are re-lexed; lexing stops where the new state matches the old one.
  - A 6 ms per-frame budget spreads long lexing over frames.
  - Recently drawn lines' spans are cached.
  - Lines over 4,000 characters are skipped without being copied out of the rope.
- **Drawing:**
  - Selections, cursors and diagnostics are filtered to the visible lines once per frame.
  - Column positions are measured incrementally, with an ASCII fast path, and very long lines jump straight to the first visible column.
  - Find highlights on very long lines only search the visible part.
  - The compiled find query is cached.
- **Problem counts are kept as running totals,** not recounted several times per frame.
- **Undo steps share their text (`Arc`)** between the undo and redo stacks, and the history keeps at most 64 MB per document.
- **The window event loop** (not measurable headless):
  - redraws only when something changed, not on every timer tick
  - doesn't redraw at all while the window is hidden, and retries a failed frame after 50 ms instead of in a busy loop
  - sets the window title only when it changes

**Pages and tests**
- **The Tests page is grouped in one pass.** Moving the cursor rebuilds it only when a failure box has to move.
- **Big pages redraw their spinner glyphs once a second,** not every 100 ms.
- **Opening an unedited test file doesn't re-parse it,** because the scan already did.
- **Running tests:** the bookkeeping uses hash sets, and results are matched to tests through an index by test name and by file.
- **Bursts of more than 20 outside changes** trigger one workspace lint, not one ESLint process per file.
- **Whole-workspace ESLint runs may take 15 minutes,** not 30 s (they used to be killed on big repositories).
- **Find in Files keeps at most 1,000 hits per file.**

**Memory**
- **The symbol index keeps no docs.** The scanner doesn't parse them for the index, and they are read again from the file when a popup shows them (the last 16 files are cached).
- **An empty `Doc` is one pointer,** down from 240 bytes inline in every symbol.
- **The index trims spare capacity** from what it keeps.
- **The npm declaration cache shares parsed files** instead of cloning them.

## Measuring memory
- **`heap_kib`** (glibc `mallinfo2`) is JDC's own data. RSS also counts the GPU driver, fonts and code.
- **Opening the stress workspace:** the symbol index of 220k symbols takes about 90 MB, and the .NET API docs index (61k APIs) about 30 MB.
