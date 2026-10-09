use serde_json::{Value, json};

/// What we want Discord to show. Produced by the MPRIS side, consumed by the
/// Discord side.
#[derive(Debug, Clone)]
pub struct Presence {
    pub player: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    pub playing: bool,
    /// Unix-epoch milliseconds at which the track would have started, given
    /// the current position. Only set while playing and position is known.
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

/// Re-reading the position always lands a few ms off, so timestamps within
/// this window count as unchanged.
const TIMESTAMP_SLACK_MS: u64 = 1500;

fn close(a: Option<u64>, b: Option<u64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.abs_diff(b) <= TIMESTAMP_SLACK_MS,
        (None, None) => true,
        _ => false,
    }
}

impl Presence {
    pub fn same_as(&self, other: &Presence) -> bool {
        self.player == other.player
            && self.title == other.title
            && self.artist == other.artist
            && self.album == other.album
            && self.art_url == other.art_url
            && self.playing == other.playing
            && close(self.start_ms, other.start_ms)
            && close(self.end_ms, other.end_ms)
    }

    /// Builds the `activity` object for Discord's SET_ACTIVITY command.
    pub fn to_activity(&self, fallback_image: Option<&str>) -> Value {
        let mut activity = json!({
            "type": 2, // "Listening to"
            "details": discord_text(&self.title),
        });

        let state = match (&self.artist, self.playing) {
            (Some(artist), true) => Some(artist.clone()),
            (Some(artist), false) => Some(format!("{artist} · paused")),
            (None, true) => None,
            (None, false) => Some("Paused".to_string()),
        };
        if let Some(state) = state {
            activity["state"] = json!(discord_text(&state));
        }

        if self.playing {
            let mut ts = json!({});
            if let Some(start) = self.start_ms {
                ts["start"] = json!(start);
            }
            if let Some(end) = self.end_ms {
                ts["end"] = json!(end);
            }
            if ts.as_object().is_some_and(|o| !o.is_empty()) {
                activity["timestamps"] = ts;
            }
        }

        let image = self
            .art_url
            .as_deref()
            .filter(|u| u.starts_with("https://") && u.len() <= 256)
            .or(fallback_image.filter(|s| !s.is_empty()));
        if let Some(image) = image {
            let hover = match &self.album {
                Some(album) => album.clone(),
                None => format!("via {}", self.player),
            };
            activity["assets"] = json!({
                "large_image": image,
                "large_text": discord_text(&hover),
            });
        }

        activity
    }
}

/// Discord rejects activity strings shorter than 2 or longer than 128 chars.
fn discord_text(s: &str) -> String {
    let mut out: String = if s.chars().count() > 128 {
        s.chars().take(127).chain(std::iter::once('…')).collect()
    } else {
        s.to_string()
    };
    while out.chars().count() < 2 {
        out.push('\u{2800}'); // braille blank: not trimmed by Discord
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_clamped() {
        assert_eq!(discord_text("a").chars().count(), 2);
        let long = "x".repeat(300);
        let clamped = discord_text(&long);
        assert_eq!(clamped.chars().count(), 128);
        assert!(clamped.ends_with('…'));
        assert_eq!(discord_text("hello"), "hello");
    }

    #[test]
    fn non_https_art_falls_back() {
        let p = Presence {
            player: "mpv".into(),
            title: "Song".into(),
            artist: None,
            album: Some("Album".into()),
            art_url: Some("file:///tmp/cover.jpg".into()),
            playing: true,
            start_ms: None,
            end_ms: None,
        };
        let a = p.to_activity(Some("fallback"));
        assert_eq!(a["assets"]["large_image"], "fallback");
        assert!(a.get("timestamps").is_none());
        let a = p.to_activity(None);
        assert!(a["assets"].get("large_image").is_none());
    }
}
