# claude-status

[![CI](https://github.com/roxblnfk/claude-status/actions/workflows/ci.yml/badge.svg)](https://github.com/roxblnfk/claude-status/actions/workflows/ci.yml)

Monitoring how fast Claude Code usage limits are being spent: a tray icon with a
ring gauge, history in SQLite, and advice on how much can still be spent today
to land exactly within the weekly window.

*[Русская версия](README.ru.md)*

## Where the data comes from

The one thing worth knowing about this project's design: **live limit
percentages are not in any file on disk.** `~/.claude/` holds only aggregates
(`stats-cache.json`) and housekeeping files; the current windows are not stored
there.

Claude Code hands the limits over in exactly one place — on stdin, to the
command configured as `statusLine.command`. The format is
[documented](https://code.claude.com/docs/en/statusline):

```json
{
  "rate_limits": {
    "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
    "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
  }
}
```

Hence the two-binary architecture:

| Binary               | Role                                                                                     |
| -------------------- | ---------------------------------------------------------------------------------------- |
| `claude-status-hook` | Registered as `statusLine.command`. Parses the JSON, appends a sample to SQLite, prints the status line. Kept light: it runs on every assistant message. |
| `claude-status`      | The statistics window and the tray icon. Reads the same database.                        |

Consequences worth remembering:

- `rate_limits` reach only Claude.ai subscribers (Pro/Max), and only after the
  first API response in a session.
- While Claude Code is not running there are no new samples. That is not a
  fault: nothing is being spent during that time either. The window shows the
  age of the latest sample.
- If a pause outlasts the window, the last reading describes a window that has
  since reset. Such a figure is not shown as current — the gauge stays empty
  and the reading is quoted separately as the last known one.
- `rate_limits` carry no per-model split — that comes from
  `~/.claude/stats-cache.json`, which Claude Code computes itself.

## Building and installing

Ready-made archives for Windows, macOS and Linux are attached to every
[release](https://github.com/roxblnfk/claude-status/releases). Or build from
source:

```bash
cargo build --release
```

Both binaries land in `target/release/` and must stay next to each other:
`claude-status` looks for the hook in its own directory.

Then launch `claude-status` and press **"Register in Claude Code"** on the
**Settings** tab. The same screen shows the current registration state and lets
you set the re-run interval.

Restart your Claude Code session afterwards.

Registration takes a backup of the settings and touches only the `statusLine`
key, leaving the order of the others intact. If a third-party command is already
there, the button offers to replace it: the previous value is preserved and
restored when the hook is removed.

The same from a shell, if you need automation:

```bash
./target/release/claude-status install     # register
./target/release/claude-status status      # check
```

## Commands

```
claude-status                    window and tray icon
claude-status install [--interval N] [--force]
claude-status uninstall
claude-status status             registration state and latest sample
claude-status preview [template] print the status line
```

`--interval N` (60 by default) makes Claude Code re-run the hook every N seconds
on top of the event-driven updates — otherwise samples go stale during pauses.

## The status line

The template is configured in the window (**Settings** tab, with a live preview,
seven ready-made presets and the placeholder reference) or directly in
`config.toml`. The default:

```
{model} · ctx {ctx_pct}% · 5h {five_bar} {five_pct}% (⟳{five_reset}) · week {week_pct}% {pace}
```

Placeholders: `{model}`, `{model_id}`, `{effort}`, `{session}`, `{dir}`,
`{cost}`, `{ctx_pct}`, `{ctx_bar}`, `{five_pct}`, `{five_bar}`, `{five_reset}`,
`{five_left}`, `{week_pct}`, `{week_bar}`, `{week_reset}`, `{week_left}`,
`{opus_pct}`, `{opus_bar}`, `{pace}`, `{daily}`, `{today_left}`, `{burn}`.

An unknown placeholder is left in the line verbatim — that is how a typo becomes
visible.

## How the spending advice is computed

Claude Code reports a percentage of the window, never tokens, so the whole
budget is computed in percent. The even pace is the diagonal from 0 % at the
start of the weekly window to 100 % at the reset:

- `{daily}` — how many percent per day may be spent to land exactly on the
  reset: `remaining / days left`.
- `{today_left}` — the same budget, scaled to the time until local midnight.
- `{pace}` — deviation from the diagonal in percentage points: `↑+12` means you
  are running 12 pp ahead of schedule.
- `{burn}` — the actual pace, %/day, averaged from the first reading of the
  current window to the last moment the newest one was still being confirmed.
  Pauses count in: a percentage point gained in four minutes and then held for
  two hours is 11 %/day, not 350. The window is identified by `resets_at`:
  points from the previous one are excluded, otherwise a reset from 100 % to
  0 % would read as a negative pace. Below half an hour of observation there is
  no pace at all — Claude Code reports whole percents, and one of them over a
  few minutes extrapolates to nonsense.

## Localisation

The interface ships in English and Russian. The language follows the operating
system by default and can be switched on the **Settings** tab, in `config.toml`
(`[ui] language = "auto" | "en" | "ru"`), or via `CLAUDE_STATUS_LANG`, which
overrides both.

Translations live in a single [`locales/app.yml`](locales/app.yml), with both
languages side by side so a missing one is obvious. Adding a language means
adding a variant to every key plus an entry in `Language` in
`crates/core/src/i18n.rs`.

## Files

| Path                                | What it is                                      |
| ----------------------------------- | ----------------------------------------------- |
| `<data>/usage.sqlite3`              | limit samples and daily tokens per model        |
| `<data>/config.toml`                | settings                                        |
| `<data>/settings.json.bak`          | backup of the Claude Code settings              |
| `<data>/previous-statusline.json`   | the third-party command displaced by `--force`  |

`<data>` is `%LOCALAPPDATA%\claude-status` on Windows,
`~/.local/share/claude-status` on Linux and
`~/Library/Application Support/claude-status` on macOS. Overridden by
`CLAUDE_STATUS_DIR`.

Samples collapse: a new row appears only when the percentages or the window
boundaries change, compared per session — otherwise the hook would pile up
duplicates on every message. They are kept for 180 days (configurable, `0` —
forever), and **Settings → Storage → Reset statistics** wipes them on demand.

## Several sessions at once

Every running Claude Code session writes its own readings, and an idle one keeps
repeating the snapshot it captured long ago — so the newest row is not the
newest data. Within a single window (same `resets_at`) usage only ever grows,
so the current value is the highest one recorded, not the last one. The same
running maximum is what the history plot draws; raw rows would zig-zag between
the true figure and a stale one.

## Environment variables

| Variable              | Purpose                                             |
| --------------------- | --------------------------------------------------- |
| `CLAUDE_CONFIG_DIR`   | Claude Code directory instead of `~/.claude`        |
| `CLAUDE_STATUS_DIR`   | directory holding the database and the configuration |
| `CLAUDE_STATUS_DUMP`  | file the hook appends raw Claude Code JSON to       |
| `CLAUDE_STATUS_LANG`  | interface language: `en`, `ru` or `auto`            |
| `CLAUDE_STATUS_DEBUG` | print hook errors to stderr                         |

`CLAUDE_STATUS_DUMP` is useful when the statusline schema grows: the weekly Opus
window, for instance, is still undocumented even though `/usage` shows it as a
separate bar. The code reads it if it arrives.

## Platforms

Windows and macOS work out of the box. On Linux the tray needs system libraries:

```bash
sudo apt install libgtk-3-dev libayatana-appindicator3-dev libxdo-dev
```

There the icon sits on top of libayatana-appindicator and needs a GTK main loop,
which winit does not provide — it is turned by hand from the paint loop. Only
tested on Windows.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
```

The hook can be exercised by hand without touching the real settings:

```bash
echo '{"model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":52,"resets_at":1800000000}}}' \
  | CLAUDE_STATUS_DIR=/tmp/cs-test ./target/debug/claude-status-hook
```

`CLAUDE_CONFIG_DIR` makes `install`/`uninstall` equally safe to test against a
throwaway directory.

### Releases

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
`feat:` bumps the minor version, `fix:` the patch one, everything else is left
out of the changelog. [release-please](https://github.com/googleapis/release-please)
keeps a pull request open with the next version and the accumulated changelog;
merging it tags the release and builds the binaries for all platforms.

The version is stored in `version.txt` and in `Cargo.toml` (the line marked
`x-release-please-version`) — both are updated by that pull request, so neither
should be edited by hand.
