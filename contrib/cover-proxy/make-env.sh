#!/bin/sh
# Writes .env for the cover proxy. Run it on the server: the password is only
# used to derive a Subsonic token and never leaves the machine.
set -eu
cd "$(dirname "$0")"

printf 'Navidrome URL from this machine [http://localhost:4533]: '
read -r nd_url
nd_url=${nd_url:-http://localhost:4533}
printf 'Navidrome user (a dedicated non-admin user is best): '
read -r ND_USER
printf 'Password: '
stty -echo; read -r nd_pass; stty echo; echo

ND_SALT=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
ND_TOKEN=$(printf '%s%s' "$nd_pass" "$ND_SALT" | md5sum | cut -d' ' -f1)
unset nd_pass

if ! curl -fsS "$nd_url/rest/ping.view?u=$ND_USER&t=$ND_TOKEN&s=$ND_SALT&v=1.16.1&c=mprissence-covers&f=json" \
    | grep -q '"status":"ok"'; then
    echo "Navidrome rejected these credentials (or isn't reachable at $nd_url)." >&2
    exit 1
fi
echo "Navidrome login OK."

printf 'Tailscale auth key (tskey-auth-...): '
stty -echo; read -r TS_AUTHKEY; stty echo; echo

umask 077
cat > .env <<ENV
ND_USER=$ND_USER
ND_TOKEN=$ND_TOKEN
ND_SALT=$ND_SALT
TS_AUTHKEY=$TS_AUTHKEY
ENV
echo "Wrote $(pwd)/.env. Start the proxy with: docker compose up -d"
