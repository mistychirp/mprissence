//! Turns a player's cover URL into one that's safe and useful to hand to
//! Discord: public, and free of credentials.

use crate::config::{Config, Navidrome};

/// Query parameters that carry credentials in media server APIs: Subsonic
/// (Navidrome, Airsonic) `u`/`p`/`t`/`s`, Jellyfin/Emby `api_key`, Plex
/// `X-Plex-Token`, and generic tokens. A false positive only costs the cover.
const SECRET_PARAMS: &[&str] = &[
    "u", "p", "t", "s", "token", "access_token", "api_key", "apikey", "x-plex-token", "jwt", "auth",
];

pub fn public_url(cfg: &Config, raw: &str) -> Option<String> {
    if let Some(url) = cfg.navidrome.as_ref().and_then(|nd| navidrome(nd, raw)) {
        return Some(url);
    }
    if has_secrets(raw) {
        return None;
    }
    Some(raw.to_string())
}

/// Maps `<server>/rest/getCoverArt.view?id=<id>&u=…&t=…` to the public cover
/// proxy, keeping only the ID.
fn navidrome(nd: &Navidrome, raw: &str) -> Option<String> {
    let rest = raw.strip_prefix(nd.server.trim_end_matches('/'))?;
    let (path, query) = rest.split_once('?')?;
    if path != "/rest/getCoverArt" && path != "/rest/getCoverArt.view" {
        return None;
    }
    let id = query.split('&').find_map(|kv| kv.strip_prefix("id="))?;
    let valid = !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| nd.cover_url.replace("{id}", id))
}

fn has_secrets(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else { return false };
    let query = query.split('#').next().unwrap_or_default();
    query.split('&').any(|kv| {
        let key = kv.split('=').next().unwrap_or_default().to_ascii_lowercase();
        SECRET_PARAMS.contains(&key.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEISHIN: &str = "http://music.lan:4533/rest/getCoverArt.view?id=7vFzGuh4Cd2kYXcmpW90el&u=me&s=salt&t=token&v=1.13.0&c=Feishin&size=300";

    fn with_navidrome() -> Config {
        Config {
            navidrome: Some(Navidrome {
                server: "http://music.lan:4533/".into(),
                cover_url: "https://covers.example.ts.net/cover/{id}".into(),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn rewrites_navidrome_covers() {
        assert_eq!(
            public_url(&with_navidrome(), FEISHIN).as_deref(),
            Some("https://covers.example.ts.net/cover/7vFzGuh4Cd2kYXcmpW90el")
        );
        // Other servers and other endpoints aren't touched.
        let other = "http://other.lan:4533/rest/getCoverArt.view?id=x&u=me&t=tok";
        assert_eq!(public_url(&with_navidrome(), other), None);
        let stream = "http://music.lan:4533/rest/stream.view?id=x&u=me&t=tok";
        assert_eq!(public_url(&with_navidrome(), stream), None);
        // IDs are passed through verbatim, so only allow safe characters.
        let weird = "http://music.lan:4533/rest/getCoverArt.view?id=../../x&u=me";
        assert_eq!(public_url(&with_navidrome(), weird), None);
    }

    #[test]
    fn refuses_urls_with_credentials() {
        let cfg = Config::default();
        assert_eq!(public_url(&cfg, FEISHIN), None);
        assert_eq!(public_url(&cfg, "https://jf.example.com/Items/1/Images/Primary?api_key=abc"), None);
        assert_eq!(public_url(&cfg, "https://plex.example.com/photo?X-Plex-Token=abc"), None);
        let cdn = "https://cdn-images.dzcdn.net/images/cover/1aa3/250x250.jpg";
        assert_eq!(public_url(&cfg, cdn).as_deref(), Some(cdn));
        let sized = "https://i.scdn.co/image/ab67?size=640";
        assert_eq!(public_url(&cfg, sized).as_deref(), Some(sized));
    }
}
