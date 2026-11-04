<p align="center"><img src="res/slate-512.png" width="128" alt="Slate"></p>

# Slate

A small, fast text editor for Windows: the everyday Notepad++ feature set in a
single ~700 KB executable with a modern, fully custom-drawn interface.

- **[Download the installer](https://github.com/dexterity-admin/slate/releases/latest)** (per-user, no admin rights) or the portable zip; Slate keeps itself up to date
- Direct2D / DirectWrite rendering, light and dark themes (follows Windows on first run), seven accent colours
- Its own vector icon set, file-type badges on tabs, smooth scrolling and gliding tab and panel transitions
- Zero CPU when idle: animations run only while something moves, and the caret is the Windows system caret, so blinking costs no repaints

## Measured against Notepad++ (same machine, same files)

`tools\bench.ps1`; `WorkingSet64` and `PrivateMemorySize64` from `Get-Process` after 26 s; idle CPU = %
of one core over 16–26 s after launch; startup CPU = CPU time used in the first 4 s.

| | WorkingSet64 | PrivateMemorySize64 | Startup CPU | Idle CPU |
|---|---|---|---|---|
| Slate, empty | **23.6 MB** | 31.7 MB | **547 ms** | **0 %** |
| Notepad++ 8.x, empty | 47.5 MB | 31.3 MB | 3,219 ms | 1.09 % |
| Slate, 2.04 MB file | **28.0 MB** | **35.0 MB** | **516 ms** | **0 %** |
| Notepad++, 2.04 MB file | 52.5 MB | 36.1 MB | 3,594 ms | 0.78 % |
| Slate, 10.2 MB file | **36.7 MB** | **43.8 MB** | **938 ms** | **0 %** |
| Notepad++, 10.2 MB file | 71.0 MB | 54.7 MB | 3,141 ms | 1.72 % |
| Executable size | **602 KB** | | | |
| notepad++.exe / full install | 8.47 MB / 31 MB | | | |

Private memory on an empty window (~31.7 MB) is the Direct2D/GPU floor that was accepted
when Direct2D was chosen; it is level with Notepad++ and Slate pulls ahead as files grow.

## Features

**Files** – open (multi-select dialog, drag-and-drop, command line), save / save as / save all,
recent files, reopen closed tab, reload, external-change detection (clean files reload silently,
modified ones ask), show in Explorer, copy path. A second `slate.exe <file>` hands the file to the
running window.

**Session** – tabs, carets, scroll positions, bookmarks and *unsaved* text are restored on the next
start. Unsaved buffers are backed up every 8 s after an edit, so a crash loses at most a few
seconds. Stored in `%APPDATA%\Slate\` (`settings.ini`, `session.ini`, `backup\`).

**Editing** – undo/redo (typing coalesced), cut/copy/paste (whole line when nothing is selected),
multiple carets (Ctrl+click, Ctrl+E next occurrence, Ctrl+Alt+Up/Down), column selection
(Alt+drag, Alt+Shift+arrows) with column copy/paste, overtype, auto-indent, smart Home,
Tab/Shift+Tab indent, toggle/add/remove line comments, duplicate, delete and move lines, join,
upper/lower/title case, sort, de-duplicate, reverse, trim trailing whitespace, tabs↔spaces,
go to matching brace, macro record and playback.

**Search** – find panel with match count, match case, whole word, regular expressions
(`$1` references in replacements), replace / replace all, find in files with folder and filters
(results open in a tab; Enter or double-click jumps to the hit), go to line[:column], and
same-word highlighting of the current selection.

**View** – tabs (drag to reorder, middle-click to close, drag onto the other view to move), split
view (move or clone a tab to the other side, draggable divider), word wrap, zoom
(Ctrl+wheel), whitespace and line-end markers, bookmarks with scrollbar ticks, code folding,
bracket matching, current-line highlight.

**Documents** – syntax highlighting for 21 languages (C/C++, C#, Java/Kotlin, JS/TS, Rust, Go,
Python, PowerShell, Batch, Shell, SQL, JSON, XML/HTML, CSS, INI/TOML, YAML, Markdown, Lua, PHP,
Visual Basic, plain text); encodings UTF-8, UTF-8 BOM, UTF-16 LE/BE and ANSI with detection,
convert-on-save and reopen-with-encoding; line endings CRLF/LF/CR, preserved on save.

Everything is reachable from the **command palette** (Ctrl+Shift+P, F1 or the ≡ button), which
also lists each shortcut. Status-bar items are clickable (line → go to line, encoding, line
ending, language, tab width, zoom, wrap).

## Keyboard shortcuts (Notepad++ layout where it has one)

| | |
|---|---|
| Ctrl+N / O / S | New / Open / Save |
| Ctrl+Alt+S / Ctrl+Shift+S | Save as / Save all |
| Ctrl+W, Ctrl+Shift+T | Close tab, reopen closed tab |
| Ctrl+Tab, Ctrl+PgUp/PgDn | Switch tabs |
| Ctrl+P, Ctrl+Shift+O | Go to open tab, open recent |
| Ctrl+Shift+P, F1 | Command palette |
| Ctrl+F / H / Shift+F | Find / Replace / Find in files |
| F3 / Shift+F3 | Find next / previous |
| Alt+C / Alt+W / Alt+R | (in find panel) match case / whole word / regex |
| Ctrl+G, Ctrl+B | Go to line, matching brace |
| Ctrl+D, Ctrl+Shift+L | Duplicate, delete line |
| Ctrl+Shift+Up/Down | Move line(s) |
| Ctrl+Q or Ctrl+/ | Toggle comment (Ctrl+K add, Ctrl+Shift+K remove) |
| Ctrl+J | Join lines |
| Ctrl+U / Ctrl+Shift+U | lowercase / UPPERCASE |
| Ctrl+E | Add next occurrence |
| Ctrl+Alt+Up/Down | Add caret above/below |
| Alt+Shift+arrows, Alt+drag | Column selection |
| Ctrl+F2, F2, Shift+F2 | Toggle / next / previous bookmark |
| Alt+0, Alt+Shift+0, Ctrl+Alt+F | Fold all, unfold all, toggle fold |
| Alt+Z | Word wrap |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom in / out / reset |
| F8 | Focus other view |
| Ctrl+Shift+R / Ctrl+Shift+M | Record / play macro |
| Insert | Overtype |

## Command line

```
slate.exe [files...] [-multiInst] [-nosession]
```

`-multiInst` starts a separate window instead of handing files to the running one;
`-nosession` neither restores nor saves the session or backups.
`slate.exe <file> <milliseconds>` is benchmark mode (closes itself, touches no settings).

## Known limits

- Only the first 32 KB of a single line is drawn (minified files with one giant line).
- Files that mix line-ending styles are unified to the dominant style when saved; the status bar
  shows "(mixed)" and a message says so on open.
- Folding follows indentation (plus Allman `{` lines), not a full parser.
- Not included, by agreement: Notepad++ plugins, function list, document map, compare,
  printing, auto-update, localization.
- Context menus, file dialogs and message boxes are the native Windows ones.
- Files up to 4 GB.

## Building

Rust (MSVC toolchain) and the Windows SDK. `build.rs` compiles `res\slate.rc` (icon, manifest,
version info) with the SDK's `rc.exe`.

```
cargo build --release
cargo test --release
```

Source layout: `doc.rs` gap buffer + line index · `buffer.rs` selections, undo, folds, bookmarks ·
`edit.rs` text commands · `syntax.rs` lexers · `enc.rs` encodings · `search.rs` find / find in
files · `app.rs` tabs, views, commands, session · `view.rs` layout cache and scrolling ·
`paint.rs` rendering · `input.rs` keyboard and mouse · `ui.rs` chrome geometry · `gfx.rs`
Direct2D resources · `sys.rs` clipboard, dialogs · `main.rs` window and message loop.

Test tools: `tools\drive.ps1` drives a live window and takes screenshots; `tools\bench.ps1`
produces the table above.
