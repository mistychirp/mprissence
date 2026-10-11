# Navidrome cover proxy

Players like Feishin report Navidrome cover art as
`http://<server>:4533/rest/getCoverArt.view?id=…&u=<user>&t=<token>&s=<salt>`.
Discord can't use that: the server is usually only reachable on your LAN,
and the URL carries your login. mprissence refuses to send it.

This proxy fixes both. It runs next to Navidrome and answers
`https://<public-host>/cover/<id>` by fetching the cover from Navidrome with
credentials that stay on the server. Nothing else is exposed: every other
path is a 404. It's made public with [Tailscale Funnel](https://tailscale.com/kb/1223/funnel),
so you don't need to open router ports or have a public IP.

Anyone who knows a cover ID can fetch that cover, and nothing more.

## Requirements

- Navidrome running in Docker Compose (the defaults assume its network is
  `navidrome_default` and the service is called `navidrome`; override with
  `ND_NETWORK` / `ND_UPSTREAM` in `.env`)
- A free [Tailscale](https://tailscale.com) account

## Setup

1. **Tailscale admin console**
   - *DNS*: enable **MagicDNS** and **HTTPS Certificates**.
   - *Access controls*: make sure the policy allows Funnel, e.g.
     ```json
     "nodeAttrs": [{ "target": ["autogroup:member"], "attr": ["funnel"] }]
     ```
     (new tailnets usually have this already).
   - *Settings → Keys*: generate an auth key.

2. **Navidrome**: create a dedicated non-admin user for the proxy (Settings →
   Users), so the stored token isn't your own login. Keep the username to
   letters and digits.

3. **On the server**, from this directory:
   ```sh
   ./make-env.sh          # asks for the Navidrome login and auth key, checks the login, writes .env
   docker compose up -d
   docker compose exec tailscale tailscale funnel status
   ```
   The last command prints your public URL, e.g.
   `https://covers.tail1234.ts.net`. The first request can take a minute
   while the certificate is issued.

4. **Check it** from any machine, with a cover ID from your player's art URL
   (the `id=` value):
   ```sh
   curl -sI https://covers.tail1234.ts.net/cover/<id>
   ```
   You should get `200` and `content-type: image/...`.

5. **Point mprissence at it** in `~/.config/mprissence/config.toml`:
   ```toml
   [navidrome]
   server = "http://192.168.1.61:4533"   # exactly as your player (Feishin) uses it
   cover_url = "https://covers.tail1234.ts.net/cover/{id}"
   ```
   or with the home-manager module:
   ```nix
   services.mprissence.settings.navidrome = {
     server = "http://192.168.1.61:4533";
     cover_url = "https://covers.tail1234.ts.net/cover/{id}";
   };
   ```

## Using your own setup instead

Nothing in mprissence is tied to this proxy or to Tailscale. `cover_url` can
point at anything that answers `GET` on that URL with an image. For example,
the same `Caddyfile` behind your own domain and reverse proxy works too.
