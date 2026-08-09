# claude-status

[![CI](https://img.shields.io/github/actions/workflow/status/roxblnfk/claude-status/ci.yml?branch=master&style=flat-square&label=CI&logo=github)](https://github.com/roxblnfk/claude-status/actions/workflows/ci.yml)
![Vibe Index](https://img.shields.io/badge/Indexing%20Vibe-6168e5?style=flat-square)

Monitoring how fast Claude Code usage limits are being spent: a tray icon with
two ring gauges — the session limit and today's budget — history in SQLite, and
advice on how much can still be spent today to land exactly within the weekly
window.

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

That command is only run by a Claude Code started from a terminal. A session
hosted by an editor over the agent SDK — Zed's agent panel, for instance — draws
no status line, so the hook never fires there. For those, and for long pauses,
Claude Code can be asked directly: see [Asking Claude Code
directly](#asking-claude-code-directly).

So one binary wears two hats:

| Invocation           | Role                                                                                     |
| -------------------- | ---------------------------------------------------------------------------------------- |
| `claude-status hook` | Registered as `statusLine.command`. Parses the JSON, appends a sample to SQLite, prints the status line. Runs on every assistant message. |
| `claude-status`      | The statistics window and the tray icon. Reads the same database.                        |

The hook used to be a binary of its own, so that the status line would not pay
for loading the whole window. A download is one file now, and the same saving
comes from delay-loading: the graphics libraries the window needs are resolved
on first use instead of at process start, which is what the import table would
otherwise do on every hook run. Measured on Windows, that is the whole of the
difference — 14.8 ms with it against 79 ms without, where the separate binary
took 13.3 ms.

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
  `~/.claude/stats-cache.json`, which Claude Code computes itself, and the
  weekly cap of a single model only from the direct request below.

## Asking Claude Code directly

Claude Code answers a control request for the data behind its `/usage` screen.
The agent SDK exposes it as
`usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET()`; on the wire it is
one line on stdin:

```bash
echo '{"type":"control_request","request_id":"r","request":{"subtype":"get_usage"}}' \
  | claude --print --input-format stream-json --output-format stream-json
```

The answer holds the five-hour and weekly windows, the weekly cap scoped to one
model — which the status line never carries — and the extra-usage credits. It is
account-wide, so any instance can be asked; it does not have to be the session
that spent anything.

The cost, measured: **3.3–4.2 s** and about **390 MB** for the duration of the
call, roughly 1.2 s of CPU. No tokens are spent — the answer reports
`total_cost_usd: 0` and no API time — but Claude Code does make one request of
its own to fetch the figures, and any `SessionStart` hooks run each time.

So the cheap source wins whenever it can. A request is made only when all of
these hold:

- it is enabled (`[probe] enabled`, on by default);
- `interval_secs` has passed since the last one (15 minutes by default, never
  less than 5 — the same throttle Claude Code applies to its own usage cache);
- and the collected readings have stopped being useful: nothing collected at
  all, nothing newer than `fresh_secs`, or the newest reading describes a window
  that has already reset.

While the status line keeps arriving, no request is made at all. `claude-status
probe` forces one, and the **Settings** tab has a button. `CLAUDE_STATUS_CLAUDE_BIN`
points at the executable if it is not where the native installer puts it.

The API is marked unstable in the SDK and may change without notice; a failed
request is reported and otherwise ignored.

## Building and installing

Ready-made archives for Windows, macOS and Linux are attached to every
[release](https://github.com/roxblnfk/claude-status/releases). Or build from
source:

```bash
cargo build --release
```

A single `claude-status` lands in `target/release/`, free to live wherever you
like — the registration records the path it was at.

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
claude-status --tray             straight into the tray, no window
claude-status install [--interval N] [--force]
claude-status uninstall
claude-status status             registration state and latest sample
claude-status preview [template] print the status line
claude-status probe              ask Claude Code for the limits and store them
claude-status hook               read one payload from stdin (what Claude Code runs)
```

`--interval N` (60 by default) makes Claude Code re-run the hook every N seconds
on top of the event-driven updates — otherwise samples go stale during pauses.

Moving or renaming the binary leaves the registration pointing at where it used
to be. That is reported as such — on the **Settings** tab and by `status` —
rather than passed off as working; registering again repairs it. The same
applies to an installation made by a release that still had a separate
`claude-status-hook`.

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
- `{today_left}` — what is left of today's ration (see below).
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

## Today's budget

Claude Code has no daily window, so one is carved out of the weekly figure.

The day starts from the level the week stood at at local midnight — the highest
reading recorded before it. Today's ration is whatever was left at that moment,
divided by the days still to come, and it is then fixed for the day: a ration
recomputed as it is spent would keep retreating and could never be reached. On
the last day of the week the division is dropped — everything still left may go
today.

This makes today's ration and `{daily}` two different numbers, and deliberately
so: `{daily}` is the same division redone at every reading, over the time left
from *now*, so it climbs whenever a day comes in under budget. Today's ration
was fixed at midnight and does not move. The one answers "what rate keeps me
going to the reset", the other "how much was set aside for today".

If collecting started later than the week did there is no reading from before
midnight, and the level has to be estimated: the usage seen at the first reading
is spread evenly back over the days since the week began. The **Today** row in
the window carries a note when the figure rests on that estimate.

## The tray icon

Two concentric gauges, the outer one inscribed in the icon bounds:

| Ring  | Shows                                                  |
| ----- | ------------------------------------------------------ |
| outer | the 5-hour session limit                                |
| inner | how much of today's budget is gone — past 100 % it caps |

Both keep their track, so a ring stays a ring at any fill and the icon never
turns into a solid disc. The colour follows the level: green, yellow past a
half, orange past three quarters, red past 90 %.

## Starting with the session

**Settings → Autostart** registers the program with the operating system: a
value under `HKCU\...\CurrentVersion\Run` on Windows, a `.desktop` file in
`~/.config/autostart` on Linux, a launch agent in `~/Library/LaunchAgents` on
macOS. The entry carries `--tray`, so a login brings up the icon and no window.

The tick is not stored in `config.toml` — it is read back from the operating
system every refresh. A copy of the answer would go stale the moment the entry
is removed by anything else. If the registered path points at a different
binary — what moving the executable leaves behind — the settings screen says so
instead of quietly reporting the autostart as off.

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
  | CLAUDE_STATUS_DIR=/tmp/cs-test ./target/debug/claude-status hook
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

The same pull request carries a refreshed [Vibe
Index](https://github.com/roxblnfk/action-vibe-index) badge — how much of the
history was written by an AI rather than by hand. It is recomputed onto the
release branch rather than onto `master`, so the figure arrives with the release
it describes instead of churning on every push.
