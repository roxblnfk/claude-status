# CLAUDE.md

Notes for future sessions. Only things that are not obvious from reading the
code — the code itself carries the rest in comments.

## What it is

A tray application watching Claude Code subscription limits. Rust workspace,
two crates:

- `crates/core` — everything that is not drawing: parsing, SQLite, the pace
  arithmetic, install, probe, update, i18n.
- `crates/app` — one binary, `claude-status`. The window and tray icon by
  default; `claude-status hook` is the status line hook, registered in
  `~/.claude/settings.json`.

**The single most important fact about the domain:** live limit percentages
exist in no file on disk. Claude Code hands them only to `statusLine.command`
on stdin, and answers a `get_usage` control request. Everything else in
`~/.claude/` is aggregates.

## Traps

**One binary, two jobs.** The hook used to be a separate binary so the status
line would not pay for loading the window. `crates/app/build.rs` marks the
graphics libraries `/DELAYLOAD` on MSVC, which is what makes the merge free:
79 ms per hook run without it, 14.8 ms with, 13.3 ms for the old separate
binary. Do not remove it without re-measuring.

**`windows_subsystem = "windows"` in release builds.** CLI subcommands print
nothing to a terminal — no console is attached — but work fine when stdout is
piped, which is how Claude Code runs the hook. Debug builds have a console, so
this only bites on release.

**Editing `locales/app.yml` may appear to do nothing.** `rust_i18n::i18n!` reads
it inside a proc macro, invisible to Cargo. `crates/core/build.rs` declares
`rerun-if-changed` on it; without that the change waits for an unrelated
rebuild.

**An unknown translation key renders as the key.** `rust-i18n` echoes it back
instead of failing, so `settings.tray.refresh` once sat in the middle of the
window unnoticed. The `translations` test in `crates/app/src/main.rs` walks the
sources for literal `tr(...)` keys and asks for each one.

**The data directory on Windows is `%APPDATA%` (roaming), not
`%LOCALAPPDATA%`** — `dirs::data_dir()`.

**Per-model and per-project tokens are counted here, not read.**
`~/.claude/stats-cache.json` carries the same numbers already aggregated, and
the plots used to take them — until Claude Code stopped recomputing the file
(it stood at `lastComputedDate: 2026-08-05` for a week while sessions ran) and
the window showed a flat line with nothing saying why. `crate::scan` reads
`~/.claude/projects/**/*.jsonl` instead. Only the lifetime counters still come
from the cache: they reach back past the oldest log on disk.

**A resumed session copies the history it continues into a new log.** In one
project 2829 assistant messages carried 1368 distinct `message.id`. Counting
lines instead of ids roughly doubles every figure — that is the likely reason
Claude Code's own daily totals run about twice ours. `counted_messages` is what
prevents it, and it also makes a scan idempotent: re-reading a log, in whole or
in part, adds nothing. Sizes are on `scanned_logs`, so only tails are parsed —
399 MB and half a minute the first time, under a second after.

**Subagent logs live one level deeper** (`<project>/<session>/subagents/`) and
on a working machine outnumbered the top-level ones two to one. A walk that
stops at the first level misses most of the tokens. Where the log sits is the
only thing that says a subagent spent it, so `usage_by_day` carries `agent` in
its key — a dimension, not a column, which is what lets the share be asked for
over any slice. Sonnet turned out to be 95 % subagent work and Opus 9 %.

**A project is the repository above the `cwd`, not the `cwd`.** Claude Code
files a session by the directory it started in, so sessions begun in
subdirectories split one checkout across a dozen entries — 44 "projects" where
there were 16. `scan::project_of` walks up to the `.git`.

**Samples are noisy by nature.** Several Claude Code sessions write at once, and
an idle one keeps repeating what it last saw. Within one window (same
`resets_at`) usage only ever grows, so the current value is the running maximum,
not the newest row. Anything reading history has to respect that.

## Working on the GUI

egui draws immediately, so a change is only real once seen. The way used all
session: temporarily point `UiState::tab()` at the tab under test, build, launch,
and screenshot the window from PowerShell — find the top-level window of the
`claude-status` process whose class is `Window Class`, `GetWindowRect`,
`CopyFromScreen`. Clicking works through `SetCursorPos` + `mouse_event`, but
coordinates go through DPI virtualisation, so read them off the captured image
rather than from `GetWindowRect`. Revert the tab default afterwards.

Verifying against real data: copy the user's database to a temp directory and
point `CLAUDE_STATUS_DIR` at it, or generate a synthetic one — a throwaway test
in core that opens `Db` and inserts rows is the quickest route.

## Conventions

Tests are named as sentences about behaviour (`a_closed_window_wakes_it_however_
fresh_the_reading`), and the comment above a test says why the case matters, not
what the code does. Comments explain rationale and trade-offs; if a comment only
restates the line below it, it should not be there.

The codebase is not `cargo fmt`-clean and CI deliberately has no fmt gate —
running it would reformat everything.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Both must be clean before committing.

## Releases

Conventional Commits; `release-please` keeps a pull request open, merging it
tags and builds. Two things ride along in that pull request: the version files
and the Vibe Index badge.

The user does not want major bumps for a desktop application — avoid `feat!:`
and `BREAKING CHANGE:` unless they ask.

Each release publishes the loose executable beside the archives; self-update
fetches that, so the asset naming in `.github/workflows/release.yml` and
`pick_asset` in `crates/core/src/update.rs` have to agree.

## Standing instructions from the user

- Commit messages end with `Assisted-By:`, **never** `Co-Authored-By:`.
- Commits are GPG-signed. If signing fails, ask — do not pass `--no-gpg-sign`.
- Their real `~/.claude/settings.json` is theirs: offer `claude-status install`
  or the button, do not edit it.
- The README is for users. Design rationale, subcommand tables and the release
  process do not belong there.
