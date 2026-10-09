use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use log::{debug, info};
use tokio::sync::watch;
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::config::Config;
use crate::presence::Presence;

const BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

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
}

#[derive(Debug, Clone, PartialEq)]
struct Track {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    art_url: Option<String>,
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

    let mut players: HashMap<String, Player> = HashMap::new();
    for name in dbus.list_names().await? {
        let Some(suffix) = name.as_str().strip_prefix(BUS_PREFIX) else { continue };
        let suffix = suffix.to_string();
        let Ok(owner) = dbus.get_name_owner(name.as_ref()).await else { continue };
        let owner = owner.to_string();
        add_player(&conn, &cfg, &mut players, suffix, owner).await;
    }
    publish(&cfg, &players, &tx);

    loop {
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
            else => anyhow::bail!("D-Bus signal streams ended"),
        }
        publish(&cfg, &players, &tx);
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
    let position_us = all.remove("Position").and_then(|v| as_u64(&v));

    if (playing && !player.playing) || track != player.track {
        player.last_active = Instant::now();
    }
    player.playing = playing;
    player.paused = paused;
    player.track = track;
    // Timestamps only mean something while playing; a paused player's
    // derived start time would drift and cause pointless updates.
    player.start_ms = position_us
        .filter(|_| playing)
        .map(|pos| now_ms.saturating_sub(pos / 1000));
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
    Some(Track { title, artist, album, art_url })
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

fn publish(cfg: &Config, players: &HashMap<String, Player>, tx: &watch::Sender<Option<Presence>>) {
    let next = select(cfg, players).and_then(|p| {
        let track = p.track.clone()?;
        Some(Presence {
            player: p.identity.clone(),
            title: track.title,
            artist: track.artist,
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
