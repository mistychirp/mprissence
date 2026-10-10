//! Full artist credits for Deezer tracks. The Deezer desktop app only puts
//! the main artist in MPRIS metadata, but its public API lists everyone.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use log::debug;
use serde::Deserialize;
use tokio::sync::mpsc;

const API: &str = "https://api.deezer.com/track/";
const TIMEOUT: Duration = Duration::from_secs(5);
/// Plenty for a listening session; cleared wholesale when exceeded.
const MAX_CACHED: usize = 1024;

pub type Lookup = (u64, Option<Vec<String>>);

pub struct Credits {
    /// `None` while a lookup is in flight or after it failed (not retried).
    cache: HashMap<u64, Option<Vec<String>>>,
    tx: mpsc::UnboundedSender<Lookup>,
}

impl Credits {
    /// Finished lookups arrive on the receiver; hand them to [`Credits::insert`].
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Lookup>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Credits { cache: HashMap::new(), tx }, rx)
    }

    /// All contributors of the Deezer track at `url`, joined for display.
    /// Starts a background lookup the first time a track is seen and returns
    /// `None` until it completes.
    pub fn artists(&mut self, url: &str) -> Option<String> {
        let id = track_id(url)?;
        if let Some(cached) = self.cache.get(&id) {
            return cached.as_ref().map(|names| names.join(", "));
        }
        if self.cache.len() >= MAX_CACHED {
            self.cache.clear();
        }
        self.cache.insert(id, None);

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let names = match tokio::task::spawn_blocking(move || fetch(id)).await {
                Ok(Ok(names)) if !names.is_empty() => Some(names),
                Ok(Ok(_)) => None,
                Ok(Err(e)) => {
                    debug!("deezer lookup for track {id}: {e:#}");
                    None
                }
                Err(_) => None,
            };
            let _ = tx.send((id, names));
        });
        None
    }

    pub fn insert(&mut self, (id, names): Lookup) {
        self.cache.insert(id, names);
    }
}

/// Extracts the track ID from URLs like `https://www.deezer.com/en/track/602456552`.
fn track_id(url: &str) -> Option<u64> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    if host != "deezer.com" && !host.ends_with(".deezer.com") {
        return None;
    }
    let (_, after) = path.split_once("track/")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[derive(Deserialize)]
struct Track {
    #[serde(default)]
    contributors: Vec<Contributor>,
}

#[derive(Deserialize)]
struct Contributor {
    name: String,
}

fn fetch(id: u64) -> Result<Vec<String>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let body = agent
        .get(format!("{API}{id}"))
        .call()?
        .body_mut()
        .read_to_string()?;
    let track: Track = serde_json::from_str(&body)?;

    let mut names: Vec<String> = Vec::new();
    for c in track.contributors {
        if !c.name.is_empty() && !names.contains(&c.name) {
            names.push(c.name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_urls() {
        assert_eq!(track_id("https://deezer.com/track/602456552"), Some(602456552));
        assert_eq!(track_id("https://www.deezer.com/en/track/2124745?x=1"), Some(2124745));
        assert_eq!(track_id("https://deezer.com/album/123"), None);
        assert_eq!(track_id("https://evil.com/deezer.com/track/1"), None);
        assert_eq!(track_id("https://notdeezer.com/track/1"), None);
    }

    #[test]
    #[ignore = "hits api.deezer.com"]
    fn fetches_all_contributors() {
        // Sunflower: MPRIS only reports Post Malone.
        assert_eq!(fetch(602456552).unwrap(), ["Post Malone", "Swae Lee"]);
    }
}
