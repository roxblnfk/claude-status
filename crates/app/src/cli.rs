//! Command line parsing.
//!
//! There are few commands and they are simple, so the parsing is hand-written —
//! an extra dependency for four subcommands does not pay for itself.

use anyhow::{Result, bail};
use claude_status_core::{
    Config, Db, autostart,
    db::Span,
    install::{self, InstallStatus},
    model_override::{self, Slot},
    paths, probe,
    render::{self, RenderContext},
    timefmt, tr, tr_args, update,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Launch the window and the tray icon. `hidden` starts in the tray alone —
    /// what autostart asks for, so that a login does not open a window over
    /// whatever the user is doing.
    Gui { hidden: bool },
    /// Register the hook in the Claude Code settings.
    Install { interval: Option<u64>, force: bool },
    /// Remove the hook from the settings.
    Uninstall,
    /// Show what is registered and what has been collected.
    Status,
    /// Print the status line from the current data.
    Preview { template: Option<String> },
    /// Ask Claude Code for the current limits and store the answer.
    Probe,
    /// Count tokens from the session logs.
    Scan,
    /// Show or change which models Claude Code uses.
    Models(ModelsAction),
    /// Read one status line payload from stdin. What Claude Code runs.
    Hook,
    /// Fetch a newer release from GitHub and put it in place.
    Update,
    Help,
}

/// What `claude-status models` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsAction {
    /// Print what is pinned, and what is questionable about it.
    Show,
    Set { slot: Slot, model: String },
    /// One slot, or every one of them.
    Clear { slot: Option<Slot> },
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<Command> {
    let args: Vec<String> = args.collect();
    let Some(first) = args.first() else {
        return Ok(Command::Gui { hidden: false });
    };

    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        autostart::TRAY_FLAG => Ok(Command::Gui { hidden: true }),
        "install" => parse_install(&args[1..]),
        "uninstall" => Ok(Command::Uninstall),
        "status" => Ok(Command::Status),
        "preview" => Ok(Command::Preview { template: args.get(1).cloned() }),
        "probe" => Ok(Command::Probe),
        "scan" => Ok(Command::Scan),
        "models" => parse_models(&args[1..]),
        install::HOOK_ARG => Ok(Command::Hook),
        "update" => Ok(Command::Update),
        other => bail!(
            "{}\n\n{}",
            tr_args("cli.unknown_command", &[("command", other)]),
            tr("cli.help")
        ),
    }
}

fn parse_install(args: &[String]) -> Result<Command> {
    let mut interval = Some(60);
    let mut force = false;
    let mut rest = args.iter();

    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--force" => force = true,
            "--no-interval" => interval = None,
            "--interval" => {
                let Some(value) = rest.next() else {
                    bail!(tr("cli.interval_needs_value"));
                };
                interval = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!(tr_args("cli.interval_not_a_number", &[("value", value)]))
                })?);
            }
            other => bail!(tr_args("cli.unknown_install_flag", &[("flag", other)])),
        }
    }
    Ok(Command::Install { interval, force })
}

fn parse_models(args: &[String]) -> Result<Command> {
    let action = match args.first().map(String::as_str) {
        None | Some("show") => ModelsAction::Show,
        Some("set") => {
            let (Some(slot), Some(model)) = (args.get(1), args.get(2)) else {
                bail!(tr("cli.models.set_usage"));
            };
            ModelsAction::Set { slot: parse_slot(slot)?, model: model.clone() }
        }
        Some("clear") => match args.get(1).map(String::as_str) {
            Some("--all") => ModelsAction::Clear { slot: None },
            Some(slot) => ModelsAction::Clear { slot: Some(parse_slot(slot)?) },
            None => bail!(tr_args("cli.models.clear_usage", &[("slots", &slot_names())])),
        },
        Some(other) => bail!(tr_args("cli.unknown_command", &[("command", other)])),
    };
    Ok(Command::Models(action))
}

fn parse_slot(name: &str) -> Result<Slot> {
    Slot::parse(name).ok_or_else(|| {
        anyhow::anyhow!(tr_args(
            "cli.models.unknown_slot",
            &[("slot", name), ("slots", &slot_names())]
        ))
    })
}

fn slot_names() -> String {
    Slot::ALL.map(Slot::name).join(", ")
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Gui { .. } | Command::Hook => unreachable!("both are dispatched from main"),
        Command::Help => {
            println!("{}", tr("cli.help"));
            Ok(())
        }
        Command::Install { interval, force } => {
            let hook = install::install(interval, force)?;
            println!("{}", tr_args("cli.installed_at", &[("path", &hook.display().to_string())]));
            println!(
                "{}",
                tr_args(
                    "cli.settings_path",
                    &[("path", &paths::claude_settings()?.display().to_string())]
                )
            );
            println!(
                "{}",
                tr_args("cli.data_path", &[("path", &paths::db_path()?.display().to_string())])
            );
            println!("\n{}", tr("cli.restart_notice"));
            Ok(())
        }
        Command::Uninstall => {
            install::uninstall()?;
            println!("{}", tr("cli.uninstalled"));
            Ok(())
        }
        Command::Status => status(),
        Command::Preview { template } => preview(template.as_deref()),
        Command::Probe => probe_once(),
        Command::Scan => scan_now(),
        Command::Models(action) => models(action),
        Command::Update => update_now(),
    }
}

/// Shows or changes which models Claude Code uses.
///
/// A change prints the state it produced rather than only confirming itself:
/// this writes into somebody else's file, and the point of seeing the result is
/// noticing what else is in there — a duplicate key, a variable in the
/// environment — before wondering why a session ignores the setting.
fn models(action: ModelsAction) -> Result<()> {
    match action {
        ModelsAction::Show => {}
        ModelsAction::Set { slot, model } => {
            let mut overrides = model_override::read()?;
            overrides.set(slot, &model);
            let path = model_override::apply(&overrides)?;

            println!(
                "{}",
                tr_args("cli.models.set_done", &[("label", &slot.label()), ("value", &model)])
            );
            println!(
                "{}",
                tr_args("settings.models.applied", &[("path", &path.display().to_string())])
            );
        }
        ModelsAction::Clear { slot: Some(slot) } => {
            let mut overrides = model_override::read()?;
            overrides.set(slot, "");
            model_override::apply(&overrides)?;
            println!("{}", tr_args("cli.models.cleared_slot", &[("label", &slot.label())]));
        }
        ModelsAction::Clear { slot: None } => {
            model_override::clear()?;
            println!("{}", tr("settings.models.cleared"));
        }
    }

    let (overrides, warnings) = model_override::read_with_warnings()?;
    let path = paths::claude_settings()?;

    println!("\n{}", tr_args("cli.models.header", &[("path", &path.display().to_string())]));
    if overrides.is_empty() {
        println!("  {}", tr("settings.models.nothing_set"));
    }
    for (slot, value) in overrides.entries() {
        println!(
            "{}",
            tr_args(
                "cli.models.row",
                &[("label", &slot.label()), ("key", slot.key()), ("value", value)]
            )
        );
    }

    if !warnings.is_empty() {
        println!("\n{}", tr("cli.models.warnings"));
        for warning in &warnings {
            println!("  {}", warning.text());
        }
    }
    if !overrides.is_empty() {
        println!("\n{}", tr("settings.models.restart_note"));
    }
    Ok(())
}

/// Checks for a newer release and installs it if there is one.
fn update_now() -> Result<()> {
    println!(
        "{}",
        tr_args(
            "settings.update.current",
            &[("version", &update::Version::current().to_string())]
        )
    );

    let Some(found) = update::check()? else {
        println!("{}", tr("update.up_to_date"));
        return Ok(());
    };
    println!(
        "{}",
        tr_args("update.available", &[("version", &found.version.to_string())])
    );

    update::install(&found)?;
    println!(
        "{}",
        tr_args("update.installed", &[("version", &found.version.to_string())])
    );
    Ok(())
}

fn status() -> Result<()> {
    match install::status()? {
        InstallStatus::Ours { command } => {
            println!("{}", tr_args("cli.status.ours", &[("command", &command)]));
        }
        InstallStatus::Stale { command } => {
            println!("{}", tr_args("cli.status.stale", &[("command", &command)]));
        }
        InstallStatus::Foreign { command } => {
            println!("{}", tr_args("cli.status.foreign", &[("command", &command)]));
        }
        InstallStatus::Absent => println!("{}", tr("cli.status.absent")),
    }
    println!(
        "{}",
        tr_args("cli.status.database", &[("path", &paths::db_path()?.display().to_string())])
    );

    let db = Db::open_default()?;
    let now = timefmt::now();
    let overview = db.overview(now)?;

    let Some(sampled_at) = overview.sampled_at else {
        println!("\n{}", tr("cli.status.no_samples"));
        return Ok(());
    };
    println!("{}", tr_args("cli.status.latest", &[("time", &timefmt::datetime(sampled_at))]));

    if let Some(w) = overview.five_hour {
        println!("{}", window_line("cli.status.five_hour", &w));
    }
    if let Some(w) = overview.week {
        println!("{}", window_line("cli.status.week", &w));
        if let Some(scoped) = overview.week_opus {
            let model = db.scoped_model()?.unwrap_or_else(|| tr("cli.status.scoped_unknown"));
            println!(
                "{}",
                tr_args(
                    "cli.status.scoped",
                    &[
                        ("model", &model),
                        ("pct", &format!("{:5.1}", scoped.used_pct)),
                        ("reset", &timefmt::datetime(scoped.resets_at)),
                    ]
                )
            );
        }
        // Past the reset the remainder belongs to a week that is over; there is
        // nothing left to divide between days.
        if !w.is_expired() {
            if let Some(per_day) = w.allowance_per_day_pct() {
                println!(
                    "{}",
                    tr_args("cli.status.allowance", &[("pct", &format!("{per_day:5.1}"))])
                );
            }
            if let Some(d) = overview.daily {
                println!(
                    "{}",
                    tr_args(
                        "cli.status.today",
                        &[
                            ("spent", &format!("{:.1}", d.spent_pct)),
                            ("allowance", &format!("{:.1}", d.allowance_pct)),
                            ("left", &format!("{:.1}", d.remaining_pct())),
                        ]
                    )
                );
            }
            if let Some(burn) = overview.week_burn {
                let pct = format!("{:5.1}", burn.pct_per_day);
                println!("{}", tr_args("cli.status.burn", &[("pct", &pct)]));
            }
        }
    }
    Ok(())
}

fn window_line(key: &str, w: &claude_status_core::WindowState) -> String {
    let Some(pct) = w.live_used_pct() else {
        return tr_args(
            &format!("{key}_expired"),
            &[("reset", &timefmt::datetime(w.resets_at))],
        );
    };
    tr_args(
        key,
        &[
            ("pct", &format!("{pct:5.1}")),
            ("reset", &timefmt::datetime(w.resets_at)),
            ("left", &timefmt::duration(w.remaining_secs())),
        ],
    )
}

/// Counts tokens from the session logs and prints what came of it.
fn scan_now() -> Result<()> {
    let mut db = Db::open_default()?;
    let report = claude_status_core::scan::run(&mut db)?;

    println!(
        "{}",
        tr_args(
            "cli.scan.done",
            &[
                ("read", &report.logs_read.to_string()),
                ("skipped", &report.logs_skipped.to_string()),
                ("messages", &report.messages.to_string()),
                ("parsed", &report.parsed.to_string()),
                ("mb", &format!("{:.1}", report.bytes as f64 / 1_048_576.0)),
            ]
        )
    );

    println!("\n{}", tr("cli.scan.models"));
    for model in db.totals_by_model(Span::ALL, None)? {
        println!("  {:<32} {:>10}", model.name, tokens(model.total()));
    }

    println!("\n{}", tr("cli.scan.projects"));
    for project in db.totals_by_project(Span::ALL)? {
        println!("  {:<48} {:>10}", project.name, tokens(project.total()));
    }
    Ok(())
}

/// `12.3B`, `354.8M`, `8260` — the same shortening the window uses.
fn tokens(n: i64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Asks Claude Code for the limits and stores what comes back.
fn probe_once() -> Result<()> {
    let usage = probe::run(std::time::Duration::from_secs(30))?;
    let db = Db::open_default()?;
    let now = timefmt::now();
    db.record_probe(&usage, now)?;
    db.set_last_probe_ts(now)?;

    println!("{}", tr("probe.updated"));
    status()
}

fn preview(template: Option<&str>) -> Result<()> {
    let config = Config::load_or_default();
    let db = Db::open_default()?;
    let now = timefmt::now();
    let overview = db.overview(now)?;

    let ctx = RenderContext {
        input: None,
        overview: &overview,
        config: &config,
        now,
    };
    let template = template.unwrap_or(&config.statusline.template);
    println!("{}", render::render_template(template, &ctx));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_launches_gui() {
        assert_eq!(parse_args(&[]).unwrap(), Command::Gui { hidden: false });
    }

    #[test]
    fn the_tray_flag_starts_without_a_window() {
        assert_eq!(parse_args(&["--tray"]).unwrap(), Command::Gui { hidden: true });
    }

    /// The argument Claude Code is given; the registered command must parse.
    #[test]
    fn the_hook_argument_is_what_gets_registered() {
        assert_eq!(parse_args(&[install::HOOK_ARG]).unwrap(), Command::Hook);
        let registered = install::command_string(std::path::Path::new("/opt/cs/claude-status"));
        assert!(registered.ends_with(&format!(" {}", install::HOOK_ARG)), "{registered}");
    }

    #[test]
    fn install_defaults_to_a_refresh_interval() {
        assert_eq!(
            parse_args(&["install"]).unwrap(),
            Command::Install { interval: Some(60), force: false }
        );
    }

    #[test]
    fn install_accepts_flags() {
        assert_eq!(
            parse_args(&["install", "--interval", "15", "--force"]).unwrap(),
            Command::Install { interval: Some(15), force: true }
        );
        assert_eq!(
            parse_args(&["install", "--no-interval"]).unwrap(),
            Command::Install { interval: None, force: false }
        );
    }

    #[test]
    fn install_rejects_bad_interval() {
        assert!(parse_args(&["install", "--interval", "often"]).is_err());
        assert!(parse_args(&["install", "--interval"]).is_err());
    }

    #[test]
    fn preview_takes_an_optional_template() {
        assert_eq!(parse_args(&["preview"]).unwrap(), Command::Preview { template: None });
        assert_eq!(
            parse_args(&["preview", "{week_pct}"]).unwrap(),
            Command::Preview { template: Some("{week_pct}".into()) }
        );
    }

    #[test]
    fn models_defaults_to_showing_what_is_set() {
        assert_eq!(parse_args(&["models"]).unwrap(), Command::Models(ModelsAction::Show));
        assert_eq!(parse_args(&["models", "show"]).unwrap(), Command::Models(ModelsAction::Show));
    }

    #[test]
    fn models_set_takes_a_slot_and_a_model() {
        assert_eq!(
            parse_args(&["models", "set", "opus", "claude-opus-4-8"]).unwrap(),
            Command::Models(ModelsAction::Set {
                slot: Slot::Opus,
                model: "claude-opus-4-8".into()
            })
        );
    }

    /// Whatever Claude Code accepts has to survive the command line, suffix and
    /// all — a value mangled on the way in would be blamed on Claude Code.
    #[test]
    fn models_set_passes_the_value_through_untouched() {
        let Command::Models(ModelsAction::Set { model, .. }) =
            parse_args(&["models", "set", "default", "claude-opus-5[1m]"]).unwrap()
        else {
            panic!("expected a set");
        };
        assert_eq!(model, "claude-opus-5[1m]");
    }

    #[test]
    fn models_clear_takes_one_slot_or_all_of_them() {
        assert_eq!(
            parse_args(&["models", "clear", "haiku"]).unwrap(),
            Command::Models(ModelsAction::Clear { slot: Some(Slot::Haiku) })
        );
        assert_eq!(
            parse_args(&["models", "clear", "--all"]).unwrap(),
            Command::Models(ModelsAction::Clear { slot: None })
        );
    }

    /// Every rejection has to name the slots, or the only way to find them is
    /// the full help.
    #[test]
    fn models_rejects_a_bad_slot_and_a_missing_value() {
        let error = parse_args(&["models", "set", "gpt", "x"]).unwrap_err().to_string();
        assert!(error.contains("subagent"), "{error}");

        assert!(parse_args(&["models", "set", "opus"]).is_err());
        assert!(parse_args(&["models", "clear"]).unwrap_err().to_string().contains("subagent"));
        assert!(parse_args(&["models", "frobnicate"]).is_err());
    }

    #[test]
    fn help_and_unknown_commands() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Command::Help);
        assert!(parse_args(&["frobnicate"]).is_err());
    }
}
