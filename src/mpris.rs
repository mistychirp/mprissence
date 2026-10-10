use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use log::{debug, info};
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until};
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::config::Config;
use crate::deezer::Credits;
use crate::presence::Presence;

const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// Some players (Deezer) announce a new track while Position keeps counting
/// from the previous one, and only reset it once the new track has buffered
/// (up to a second or two later, without a signal). Until then we assume the
/// track started at the change and poll at this interval...
const STALE_RECHECK: Duration = Duration::from_millis(250);
/// ...giving up and trusting the player after this long.
const STALE_WINDOW: Duration = Duration::from_secs(5);
/// Start times this close together are the same clock.
const SAME_START_MS: u64 = 1500;
/// Position changes never emit signals, and not every player sends Seeked,
/// so re-read playing players this often to catch drift.
const RESYNC_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Player {
    /// Unique bus name (":1.42") currently owning the well-known name.
    owner: String,
    identity: String,
    playing: bool,
    paused: bool,
    track: Option<Track>,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    /// When this player last started playing or changed track; the most
    /// recent one wins when several are playing.
    last_active: Instant,
    /// When to re-read this player even without a signal.
    recheck_at: Option<Instant>,
    /// Set after a track change while Position may still be the old track's.
    stale: Option<StalePosition>,
}

#[derive(Debug)]
struct StalePosition {
    /// The previous track's start time; a position still counting from it is stale.
    old_start_ms: u64,
    /// When the new track was announced, used as its start in the meantime.
    assumed_start_ms: u64,
    until: Instant,
}

#[derive(Debug, Clone, PartialEq)]
struct Track {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    art_url: Option<String>,
    url: Option<String>,
}

/// Watches the session bus for MPRIS players and publishes the presence of
/// whichever one should be shown.
pub async fn run(cfg: Config, tx: watch::Sender<Option<Presence>>) -> Result<()> {
    let conn = Connection::session().await.context("connecting to the session bus")?;
    let dbus = DBusProxy::new(&conn).await?;

    // Subscribe before listing players so nothing slips through in between.
    let mut owner_changes = dbus.receive_name_owner_changed().await?;
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(OBJECT_PATH)?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &conn, None).await?;

    let (mut credits, mut lookups) = Credits::new();
    let mut players: HashMap<String, Player> = HashMap::new();
    for name in dbus.list_names().await? {
        let Some(suffix) = name.as_str().strip_prefix(BUS_PREFIX) else { continue };
        let suffix = suffix.to_string();
        let Ok(owner) = dbus.get_name_owner(name.as_ref()).await else { continue };
        let owner = owner.to_string();
        add_player(&conn, &cfg, &mut players, suffix, owner).await;
    }
    publish(&cfg, &players, &mut credits, &tx);

    loop {
        let next_recheck = players.values().filter_map(|p| p.recheck_at).min();
        tokio::select! {
            Some(change) = owner_changes.next() => {
                let args = change.args()?;
                let Some(suffix) = args.name().as_str().strip_prefix(BUS_PREFIX) else { continue };
                let suffix = suffix.to_string();
                match args.new_owner().as_ref() {
                    Some(owner) => add_player(&conn, &cfg, &mut players, suffix, owner.to_string()).await,
                    None => {
                        if players.remove(&suffix).is_some() {
                            info!("player gone: {suffix}");
                        }
                    }
                }
            }
            Some(msg) = signals.next() => {
                let Ok(msg) = msg else { continue };
                let header = msg.header();
                let Some(sender) = header.sender() else { continue };
                // PropertiesChanged and Seeked both just mean "re-read the player".
                for (suffix, player) in players.iter_mut().filter(|(_, p)| p.owner == sender.as_str()) {
                    if let Err(e) = refresh(&conn, suffix, player).await {
                        debug!("refreshing {suffix}: {e:#}");
                    }
                }
            }
            _ = sleep_until(next_recheck.unwrap_or_else(Instant::now)), if next_recheck.is_some() => {
                let now = Instant::now();
                for (suffix, player) in players.iter_mut().filter(|(_, p)| p.recheck_at.is_some_and(|t| t <= now)) {
                    if let Err(e) = refresh(&conn, suffix, player).await {
                        debug!("rechecking {suffix}: {e:#}");
                        player.recheck_at = None;
                    }
                }
            }
            Some(lookup) = lookups.recv() => credits.insert(lookup),
            else => anyhow::bail!("D-Bus signal streams ended"),
        }
        publish(&cfg, &players, &mut credits, &tx);
    }
}

async fn add_player(
    conn: &Connection,
    cfg: &Config,
    players: &mut HashMap<String, Player>,
    suffix: String,
    owner: String,
) {
    let props = match properties(conn, &suffix).await {
        Ok(p) => p,
        Err(e) => return debug!("{suffix}: {e:#}"),
    };
    let identity = props
        .get(InterfaceName::from_static_str_unchecked(ROOT_IFACE), "Identity")
        .await
        .ok()
        .and_then(|v| String::try_from(v).ok())
        .unwrap_or_else(|| suffix.clone());

    if cfg.is_ignored(&suffix, &identity) {
        return debug!("ignoring player {suffix} ({identity})");
    }
    info!("player found: {suffix} ({identity})");

    let mut player = Player {
        owner,
        identity,
        playing: false,
        paused: false,
        track: None,
        start_ms: None,
        end_ms: None,
        last_active: Instant::now(),
        recheck_at: None,
        stale: None,
    };
    if let Err(e) = refresh(conn, &suffix, &mut player).await {
        debug!("refreshing {suffix}: {e:#}");
    }
    players.insert(suffix, player);
}

async fn properties<'a>(conn: &Connection, suffix: &str) -> Result<PropertiesProxy<'a>> {
    let dest = BusName::try_from(format!("{BUS_PREFIX}{suffix}"))?;
    Ok(PropertiesProxy::builder(conn)
        .destination(dest)?
        .path(OBJECT_PATH)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?)
}

async fn refresh(conn: &Connection, suffix: &str, player: &mut Player) -> Result<()> {
    let props = properties(conn, suffix).await?;
    let mut all = props
        .get_all(InterfaceName::from_static_str_unchecked(PLAYER_IFACE))
        .await?;
    let now_ms = epoch_ms();

    let status = all.remove("PlaybackStatus").and_then(|v| String::try_from(v).ok());
    let playing = status.as_deref() == Some("Playing");
    let paused = status.as_deref() == Some("Paused");

    let metadata: HashMap<String, OwnedValue> = all
        .remove("Metadata")
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default();
    let track = parse_track(&metadata);
    let length_us = metadata.get("mpris:length").and_then(|v| as_u64(v));
    let position_us = match (all.remove("Position").and_then(|v| as_u64(&v)), length_us) {
        // A position past the end can't be this track's.
        (Some(pos), Some(len)) if len > 0 && pos > len => Some(0),
        (pos, _) => pos,
    };
    // Timestamps only mean something while playing; a paused player's
    // derived start time would drift and cause pointless updates.
    let reported_start = position_us
        .filter(|_| playing)
        .map(|pos| now_ms.saturating_sub(pos / 1000));

    let track_changed = track != player.track;
    if (playing && !player.playing) || track_changed {
        player.last_active = Instant::now();
    }
    if track_changed && player.stale.is_none() {
        player.stale = player.start_ms.map(|old_start_ms| StalePosition {
            old_start_ms,
            assumed_start_ms: now_ms,
            until: Instant::now() + STALE_WINDOW,
        });
    }
    let start_ms = match (&player.stale, reported_start) {
        (Some(stale), Some(reported))
            if Instant::now() < stale.until && reported.abs_diff(stale.old_start_ms) <= SAME_START_MS =>
        {
            Some(stale.assumed_start_ms)
        }
        _ => {
            player.stale = None;
            reported_start
        }
    };

    player.recheck_at = playing.then(|| {
        Instant::now() + if player.stale.is_some() { STALE_RECHECK } else { RESYNC_INTERVAL }
    });
    player.playing = playing;
    player.paused = paused;
    player.track = track;
    player.start_ms = start_ms;
    player.end_ms = match (player.start_ms, length_us) {
        (Some(start), Some(len)) if len > 0 => Some(start + len / 1000),
        _ => None,
    };
    Ok(())
}

fn parse_track(md: &HashMap<String, OwnedValue>) -> Option<Track> {
    let title = md.get("xesam:title").and_then(|v| as_string(v)).filter(|s| !s.trim().is_empty())?;
    let artist = md
        .get("xesam:artist")
        .and_then(|v| as_string_list(v))
        .map(|a| a.join(", "))
        .filter(|s| !s.trim().is_empty());
    let album = md.get("xesam:album").and_then(|v| as_string(v)).filter(|s| !s.trim().is_empty());
    let art_url = md.get("mpris:artUrl").and_then(|v| as_string(v)).filter(|s| !s.is_empty());
    let url = md.get("xesam:url").and_then(|v| as_string(v)).filter(|s| !s.is_empty());
    Some(Track { title, artist, album, art_url, url })
}

/// Picks the player to show: the most recently active one that's playing,
/// or (with `show_paused`) the most recently active paused one.
fn select<'a>(cfg: &Config, players: &'a HashMap<String, Player>) -> Option<&'a Player> {
    let candidates = players.values().filter(|p| p.track.is_some());
    let playing = candidates.clone().filter(|p| p.playing).max_by_key(|p| p.last_active);
    playing.or_else(|| {
        cfg.show_paused
            .then(|| candidates.filter(|p| p.paused).max_by_key(|p| p.last_active))
            .flatten()
    })
}

fn publish(
    cfg: &Config,
    players: &HashMap<String, Player>,
    credits: &mut Credits,
    tx: &watch::Sender<Option<Presence>>,
) {
    let next = select(cfg, players).and_then(|p| {
        let track = p.track.clone()?;
        let artist = track
            .url
            .as_deref()
            .filter(|_| cfg.deezer_contributors)
            .and_then(|url| credits.artists(url))
            .or(track.artist);
        Some(Presence {
            player: p.identity.clone(),
            title: track.title,
            artist,
            album: track.album,
            art_url: track.art_url,
            playing: p.playing,
            start_ms: p.start_ms,
            end_ms: p.end_ms,
        })
    });
    tx.send_if_modified(|current| {
        let same = match (&*current, &next) {
            (Some(a), Some(b)) => a.same_as(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            *current = next;
        }
        !same
    });
}

fn epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Metadata values arrive wrapped in variants, sometimes nested.
fn peel<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => peel(inner),
        other => other,
    }
}

fn as_string(v: &Value) -> Option<String> {
    match peel(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn as_string_list(v: &Value) -> Option<Vec<String>> {
    match peel(v) {
        Value::Array(a) => Some(a.iter().filter_map(as_string).collect()),
        // Some players send a single string where the spec says a list.
        other => as_string(other).map(|s| vec![s]),
    }
}

/// Lengths and positions are spec'd as i64 but some players send u64 or i32.
fn as_u64(v: &Value) -> Option<u64> {
    match peel(v) {
        Value::I64(n) => u64::try_from(*n).ok(),
        Value::U64(n) => Some(*n),
        Value::I32(n) => u64::try_from(*n).ok(),
        Value::U32(n) => Some(u64::from(*n)),
        Value::F64(n) if *n >= 0.0 => Some(*n as u64),
        _ => None,
    }
}
