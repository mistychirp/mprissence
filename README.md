# mprissence

Discord Rich Presence for any Linux media player that speaks
[MPRIS](https://specifications.freedesktop.org/mpris-spec/latest/): Spotify,
Deezer, mpv, VLC, Elisa, Strawberry, browsers, and so on.

It runs as a small background daemon in your user session and shows what
you're playing as **Listening to …**, with the title, artist, album art, and a
live progress bar.

- Event-driven: reacts to D-Bus signals instead of polling every second
- Picks the right player when several are open (the most recently active one),
  with an ignore list for the ones you never want shown
- Works with the official Discord client (native, Flatpak, Snap) and with
  Vesktop/arRPC
- Reconnects on its own when Discord is restarted, and stays under Discord's
  rate limit when you skip through tracks
- Optional: full artist credits for Deezer tracks, which the Deezer app itself
  leaves out

## Before you install: create a Discord application

Discord shows an activity under an *application*, so you need one (it's free
and takes a minute):

1. Go to <https://discord.com/developers/applications> and click
   **New Application**. Its name is what appears on your activity card, so
   pick something like "Music".
2. Copy the **Application ID** from the **General Information** page. That
   number is your `client_id`.
3. *(Optional)* Under **Rich Presence → Art Assets**, upload images you want
   to use for `fallback_image` or `small_image` (see
   [Configuration](#configuration)). New assets can take a few minutes to
   become available.

Also make sure **Settings → Activity Privacy → Share your detected activities
with others** is on in Discord, or nobody will see it.

## Installation

### Nix / NixOS

The flake provides a package and a [home-manager](https://github.com/nix-community/home-manager)
module that installs mprissence, writes its config and sets up the systemd
user service.

Add the input to your flake:

```nix
inputs.mprissence = {
  url = "github:mistychirp/mprissence";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then in your home-manager configuration (pass `inputs` through
`extraSpecialArgs` if you haven't already):

```nix
{ inputs, ... }:
{
  imports = [ inputs.mprissence.homeManagerModules.default ];

  services.mprissence = {
    enable = true;
    settings = {
      client_id = "123456789012345678";
      show_paused = true;
    };
  };
}
```

`settings` takes the same keys as the [config file](#configuration). Rebuild
and the service starts with your graphical session.

To pull in a newer version later, run `nix flake update mprissence` before
rebuilding.

Just want to try it first?

```sh
nix run github:mistychirp/mprissence -- --dry-run
```

### Arch Linux

There's no AUR package yet, so build it with cargo:

```sh
sudo pacman -S --needed base-devel git rust
git clone https://github.com/mistychirp/mprissence.git
cd mprissence
cargo install --path .
```

Then follow [Setting up the service](#setting-up-the-service-non-nix).

### Debian / Ubuntu

mprissence needs Rust 1.89 or newer, which is usually newer than what
Debian and Ubuntu package, so install it with [rustup](https://rustup.rs)
instead of apt:

```sh
sudo apt install build-essential git curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

git clone https://github.com/mistychirp/mprissence.git
cd mprissence
cargo install --path .
```

Then follow [Setting up the service](#setting-up-the-service-non-nix).

### Other distros

Anything with systemd and a C compiler works the same way: install Rust
(through your package manager if it's 1.89+, otherwise rustup), then
`cargo install --path .` from a clone. No other system libraries are needed.

### Setting up the service (non-Nix)

`cargo install` puts the binary in `~/.cargo/bin/mprissence`. Create the
config and enable the bundled systemd user service from inside the clone:

```sh
mkdir -p ~/.config/mprissence
cp config.example.toml ~/.config/mprissence/config.toml
# edit ~/.config/mprissence/config.toml and set client_id

mkdir -p ~/.config/systemd/user
cp contrib/mprissence.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now mprissence
```

It now starts every time you log in. To update later, `git pull` and run
`cargo install --path .` again, then `systemctl --user restart mprissence`.

## Configuration

The config file lives at `~/.config/mprissence/config.toml` (or
`$XDG_CONFIG_HOME/mprissence/config.toml`). See
[`config.example.toml`](config.example.toml) for a commented version.

| Key | Default | Description |
| --- | --- | --- |
| `client_id` | *required* | Your Discord Application ID, as a quoted string. |
| `display` | `"artist"` | What follows "Listening to" in the member list: `"artist"`, `"title"`, or `"name"` (the application's name). |
| `ignore` | `[]` | Players to skip, matched case-insensitively against the MPRIS bus name or the player's display name, e.g. `["firefox", "chromium"]`. |
| `show_paused` | `false` | Keep the activity up while paused (without the progress bar). |
| `fallback_image` | none | Image used when a track has no usable cover art. An `https://` URL or an Art Assets key. |
| `small_image` | none | Small badge over the cover art. An `https://` URL or an Art Assets key. |
| `small_text` | none | Tooltip for the small badge. |
| `deezer_contributors` | `false` | Fetch full artist credits for Deezer tracks from `api.deezer.com` (one request per track). |
| `[navidrome]` | none | `server` and `cover_url`: map Navidrome cover links (e.g. from Feishin) to a public cover proxy. See [Album art](#album-art). |

Example:

```toml
client_id = "123456789012345678"
display = "title"
ignore = ["firefox", "chromium"]
show_paused = true
small_image = "my-logo"
small_text = "Listening on Linux"
```

After changing the file, restart the service:
`systemctl --user restart mprissence`. (With the home-manager module, a
rebuild restarts it for you.)

### Album art

Discord's servers download the cover themselves, so it has to be a public
`https://` URL. Streaming players like Spotify and Deezer usually provide one.
Others don't:

- **Local players and browsers** (mpv, Elisa, Firefox, Chromium) only have a
  `file://` path, which mprissence won't upload anywhere. `fallback_image` is
  shown instead.
- **Self-hosted servers** (Navidrome via Feishin and other Subsonic clients,
  Jellyfin, Plex) usually point at a LAN address and put your login in the
  URL. mprissence never passes URLs with credentials (`u`/`t`/`s`, `api_key`,
  `X-Plex-Token`, …) to Discord.

For Navidrome, [`contrib/cover-proxy`](contrib/cover-proxy) is a small
proxy you can run next to your server. It publishes covers at
`https://<host>/cover/<id>` without your login. Point the `[navidrome]`
section at it:

```toml
[navidrome]
server = "http://192.168.1.61:4533"   # exactly as your player uses it
cover_url = "https://covers.example.ts.net/cover/{id}"
```

`cover_url` can be any URL that serves the image for `{id}`, so you can use
your own proxy instead.

## Usage

```
mprissence [--config PATH] [--dry-run]

  -c, --config PATH  config file (default: $XDG_CONFIG_HOME/mprissence/config.toml)
      --dry-run      print what would be shown instead of talking to Discord
  -h, --help         show this help
  -V, --version      show the version
```

`--dry-run` is the quickest way to check that your player is detected. It
doesn't need Discord or a config file:

```sh
mprissence --dry-run
```

Logs go to the journal when running as a service:

```sh
journalctl --user -u mprissence -f
```

Set `RUST_LOG=mprissence=debug` for more detail.

## Troubleshooting

**Nothing shows up on Discord.**
Run `mprissence --dry-run` while music is playing. If it prints
`(no activity)`, check that your player shows up in
`busctl --user list | grep mpris` and isn't matched by `ignore`. If the dry
run looks right, check the logs for `connected to Discord` and make sure
activity sharing is enabled in Discord's privacy settings.

**The service doesn't start at login.**
It's tied to `graphical-session.target`. GNOME and KDE activate it, but some
standalone window managers don't unless they're started through a systemd
session (e.g. `uwsm` for Hyprland or `niri-session` for niri). Check with
`systemctl --user is-active graphical-session.target`.

**`invalid type: integer, expected a string`**
Put quotes around `client_id`: `client_id = "123456789012345678"`.

**The service fails right after updating.**
An unknown key in the config is an error, so if you added a setting that
your installed version doesn't have yet, update mprissence first.
