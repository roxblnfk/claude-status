//! Command line parsing.
//!
//! There are few commands and they are simple, so the parsing is hand-written —
//! an extra dependency for four subcommands does not pay for itself.

use anyhow::{Result, bail};
use claude_status_core::{
    Config, Db,
    install::{self, InstallStatus},
    paths,
    render::{self, RenderContext},
    timefmt, tr, tr_args,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Launch the window and the tray icon.
    Gui,
    /// Register the hook in the Claude Code settings.
    Install { interval: Option<u64>, force: bool },
    /// Remove the hook from the settings.
    Uninstall,
    /// Show what is registered and what has been collected.
    Status,
    /// Print the status line from the current data.
    Preview { template: Option<String> },
    Help,
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<Command> {
    let args: Vec<String> = args.collect();
    let Some(first) = args.first() else {
        return Ok(Command::Gui);
    };

    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "install" => parse_install(&args[1..]),
        "uninstall" => Ok(Command::Uninstall),
        "status" => Ok(Command::Status),
        "preview" => Ok(Command::Preview { template: args.get(1).cloned() }),
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

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Gui => unreachable!("the GUI is launched from main"),
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
    }
}

fn status() -> Result<()> {
    match install::status()? {
        InstallStatus::Ours { command } => {
            println!("{}", tr_args("cli.status.ours", &[("command", &command)]));
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
        assert_eq!(parse_args(&[]).unwrap(), Command::Gui);
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
    fn help_and_unknown_commands() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Command::Help);
        assert!(parse_args(&["frobnicate"]).is_err());
    }
}
