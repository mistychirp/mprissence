use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use log::{debug, info, warn};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep, sleep_until};

use crate::config::Config;
use crate::presence::Presence;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

/// Wait this long after a change before sending, so skipping through tracks
/// sends one update instead of many.
const DEBOUNCE: Duration = Duration::from_millis(750);
/// Discord allows roughly 5 activity updates per 20 seconds.
const RATE_LIMIT: (usize, Duration) = (5, Duration::from_secs(20));
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Keeps a connection to the local Discord client and mirrors `rx` into the
/// user's activity. Never returns; reconnects whenever Discord goes away.
pub async fn run(cfg: Config, mut rx: watch::Receiver<Option<Presence>>) -> ! {
    let mut backoff = RECONNECT_MIN;
    loop {
        match connect(&cfg.client_id).await {
            Ok((reader, writer)) => {
                info!("connected to Discord");
                backoff = RECONNECT_MIN;
                if let Err(e) = serve(&cfg, reader, writer, &mut rx).await {
                    warn!("Discord connection lost: {e:#}");
                }
            }
            Err(e) => debug!("Discord not available: {e:#}"),
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

fn socket_candidates() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|var| std::env::var_os(var).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .collect();
    bases.push(PathBuf::from("/tmp"));

    // Native, Flatpak (Discord, Vesktop), and Snap installs.
    let subdirs = [
        "",
        "app/com.discordapp.Discord",
        "app/com.discordapp.DiscordCanary",
        "app/dev.vencord.Vesktop",
        ".flatpak/dev.vencord.Vesktop/xdg-run",
        "snap.discord",
        "snap.discord-canary",
    ];

    let mut out = Vec::new();
    for base in &bases {
        for sub in subdirs {
            for i in 0..10 {
                let path = base.join(sub).join(format!("discord-ipc-{i}"));
                if !out.contains(&path) {
                    out.push(path);
                }
            }
        }
    }
    out
}

async fn connect(client_id: &str) -> Result<(OwnedReadHalf, OwnedWriteHalf)> {
    for path in socket_candidates() {
        let Ok(stream) = UnixStream::connect(&path).await else { continue };
        debug!("trying {}", path.display());
        let (mut reader, mut writer) = stream.into_split();
        write_frame(&mut writer, OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id })).await?;
        let (op, body) = read_frame(&mut reader).await?;
        match op {
            OP_FRAME if body["evt"] == "READY" => {
                if let Some(user) = body["data"]["user"]["username"].as_str() {
                    info!("Discord user: {user}");
                }
                return Ok((reader, writer));
            }
            OP_CLOSE => bail!("Discord refused the handshake: {}", body["message"]),
            _ => bail!("unexpected handshake reply (op {op}): {body}"),
        }
    }
    bail!("no Discord IPC socket found")
}

enum Inbound {
    Ping(Value),
}

async fn serve(
    cfg: &Config,
    reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    rx: &mut watch::Receiver<Option<Presence>>,
) -> Result<()> {
    // Reading happens on its own task so a half-read frame is never dropped
    // by select!. The channel closing means the socket did.
    let (in_tx, mut in_rx) = mpsc::channel(8);
    let reader_task = tokio::spawn(read_loop(reader, in_tx));
    let _abort = AbortOnDrop(reader_task);

    let mut sent: VecDeque<Instant> = VecDeque::new();
    let mut nonce: u64 = 0;
    // Push the current state right away on (re)connect.
    rx.mark_changed();

    loop {
        tokio::select! {
            inbound = in_rx.recv() => match inbound {
                Some(Inbound::Ping(body)) => write_frame(&mut writer, OP_PONG, &body).await?,
                None => bail!("socket closed"),
            },
            changed = rx.changed() => {
                changed.map_err(|_| anyhow!("presence source stopped"))?;
                sleep(DEBOUNCE).await;

                while sent.len() >= RATE_LIMIT.0 {
                    let oldest = sent[0];
                    if oldest.elapsed() >= RATE_LIMIT.1 {
                        sent.pop_front();
                    } else {
                        sleep_until(oldest + RATE_LIMIT.1).await;
                    }
                }

                let presence = rx.borrow_and_update().clone();
                let activity = match &presence {
                    Some(p) => {
                        info!("now showing: {} — {}", p.title, p.artist.as_deref().unwrap_or("?"));
                        p.to_activity(cfg.fallback_image.as_deref())
                    }
                    None => {
                        info!("clearing activity");
                        Value::Null
                    }
                };
                nonce += 1;
                let cmd = json!({
                    "cmd": "SET_ACTIVITY",
                    "args": { "pid": std::process::id(), "activity": activity },
                    "nonce": nonce.to_string(),
                });
                write_frame(&mut writer, OP_FRAME, &cmd).await?;
                sent.push_back(Instant::now());
            }
        }
    }
}

async fn read_loop(mut reader: OwnedReadHalf, tx: mpsc::Sender<Inbound>) {
    loop {
        let (op, body) = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(e) => return debug!("read: {e:#}"),
        };
        match op {
            OP_FRAME if body["evt"] == "ERROR" => {
                warn!("Discord rejected an update: {}", body["data"]["message"]);
            }
            OP_FRAME => debug!("discord: {body}"),
            OP_PING => {
                if tx.send(Inbound::Ping(body)).await.is_err() {
                    return;
                }
            }
            OP_CLOSE => return debug!("Discord closed the connection: {body}"),
            _ => debug!("ignoring op {op}: {body}"),
        }
    }
}

async fn write_frame(w: &mut OwnedWriteHalf, op: u32, body: &Value) -> Result<()> {
    let body = serde_json::to_vec(body)?;
    let mut buf = Vec::with_capacity(8 + body.len());
    buf.extend_from_slice(&op.to_le_bytes());
    buf.extend_from_slice(&u32::try_from(body.len())?.to_le_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf).await.context("writing to Discord")
}

async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> Result<(u32, Value)> {
    let mut header = [0u8; 8];
    r.read_exact(&mut header).await.context("reading from Discord")?;
    let op = u32::from_le_bytes(header[..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    if len > 1 << 20 {
        bail!("oversized frame ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.context("reading from Discord")?;
    Ok((op, serde_json::from_slice(&body)?))
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
