mod config;
mod discord;
mod mpris;
mod presence;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use log::info;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;

use config::Config;

const USAGE: &str = "\
mprissence — Discord rich presence for MPRIS media players

Usage: mprissence [--config PATH] [--dry-run]

  -c, --config PATH  config file (default: $XDG_CONFIG_HOME/mprissence/config.toml)
      --dry-run      print what would be shown instead of talking to Discord
  -h, --help         show this help
  -V, --version      show the version

Log verbosity follows RUST_LOG (default: info).";

struct Args {
    config: Option<PathBuf>,
    dry_run: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args { config: None, dry_run: false };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-c" | "--config") => {
                args.config = Some(it.next().context("--config needs a path")?.into());
            }
            Some("--dry-run") => args.dry_run = true,
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some("-V" | "--version") => {
                println!("mprissence {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => bail!("unknown argument {arg:?}\n\n{USAGE}"),
        }
    }
    Ok(args)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None) // journald adds its own
        .init();

    let args = parse_args()?;
    let path = match args.config {
        Some(p) => p,
        None => Config::default_path()?,
    };
    let cfg = if args.dry_run && !path.exists() {
        Config::default()
    } else {
        Config::load(&path)?
    };

    let (tx, rx) = watch::channel(None);
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        res = mpris::run(cfg.clone(), tx) => res?,
        _ = output(cfg, rx, args.dry_run) => {}
        _ = sigterm.recv() => info!("SIGTERM, exiting"),
        _ = sigint.recv() => info!("SIGINT, exiting"),
    }
    // Discord clears the activity by itself when our socket closes.
    Ok(())
}

async fn output(cfg: Config, mut rx: watch::Receiver<Option<presence::Presence>>, dry_run: bool) {
    if !dry_run {
        discord::run(cfg, rx).await;
    }
    loop {
        if let Some(p) = rx.borrow_and_update().clone() {
            let activity = p.to_activity(cfg.fallback_image.as_deref());
            println!("{}", serde_json::to_string_pretty(&activity).unwrap());
        } else {
            println!("(no activity)");
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}
