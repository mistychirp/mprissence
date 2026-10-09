use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Discord application ID. The application's name is the fallback for
    /// "Listening to <name>" and is shown on the activity card.
    pub client_id: String,

    /// What follows "Listening to" in the member list and status line.
    #[serde(default)]
    pub display: Display,

    /// Players to ignore, matched case-insensitively as a substring of the
    /// MPRIS bus name (e.g. "firefox") or the player's Identity.
    #[serde(default)]
    pub ignore: Vec<String>,

    /// Keep showing the presence (without a progress bar) while paused.
    #[serde(default)]
    pub show_paused: bool,

    /// Asset key or https URL shown when the track has no usable cover art.
    #[serde(default)]
    pub fallback_image: Option<String>,

    /// Asset key or https URL for the small badge over the cover art.
    #[serde(default)]
    pub small_image: Option<String>,

    /// Tooltip for the small badge.
    #[serde(default)]
    pub small_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Display {
    /// The Discord application's name.
    Name,
    #[default]
    Artist,
    Title,
}

impl Config {
    pub fn default_path() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => {
                let home = std::env::var_os("HOME").context("neither XDG_CONFIG_HOME nor HOME is set")?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(base.join("mprissence").join("config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        if cfg.client_id.is_empty() || !cfg.client_id.bytes().all(|b| b.is_ascii_digit()) {
            bail!("client_id must be the numeric application ID from https://discord.com/developers/applications");
        }
        Ok(cfg)
    }

    pub fn is_ignored(&self, bus_suffix: &str, identity: &str) -> bool {
        let bus_suffix = bus_suffix.to_lowercase();
        let identity = identity.to_lowercase();
        self.ignore.iter().any(|pat| {
            let pat = pat.to_lowercase();
            bus_suffix.contains(&pat) || identity.contains(&pat)
        })
    }
}
