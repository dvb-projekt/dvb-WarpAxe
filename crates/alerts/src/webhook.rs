//! The webhook URL: taken only when it is one of Discord's.
//!
//! Discord's "Copy Webhook URL" gives
//! `https://discord.com/api/webhooks/<id>/<token>`. The token is the
//! secret: whoever has the URL can post to the channel. So the miner
//! takes nothing else (no other host a typo or a stranger's page could
//! send the token to, no plain http, no user or port in the URL), and
//! the firmware never shows the URL back (`discordWebhookSet` instead).

/// Discord's hosts for webhooks: the stable app, its old domain, and the
/// test builds' (canary, ptb).
pub const HOSTS: &[&str] = &[
    "discord.com",
    "discordapp.com",
    "canary.discord.com",
    "ptb.discord.com",
    "canary.discordapp.com",
    "ptb.discordapp.com",
];

/// A stored URL is at most this long (Discord's are about 120).
pub const MAX_LEN: usize = 256;

/// Why a URL was refused, as the API says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Longer than [`MAX_LEN`].
    TooLong,
    /// A space, a control character or anything outside printable ASCII.
    BadCharacter,
    /// Not `https://`.
    NotHttps,
    /// A user, a password or a port in the URL.
    UserOrPort,
    /// Not one of [`HOSTS`].
    NotDiscord,
    /// Not `/api/webhooks/<id>/<token>` (an optional `?thread_id=<id>`
    /// aside).
    NotWebhook,
}

impl Refused {
    /// The error text for the API.
    pub fn text(self) -> &'static str {
        match self {
            Refused::TooLong => "discordWebhook: too long for a Discord webhook URL",
            Refused::BadCharacter => {
                "discordWebhook: contains spaces or characters a URL cannot have"
            }
            Refused::NotHttps => "discordWebhook: must start with https://",
            Refused::UserOrPort => "discordWebhook: a user name or port is not allowed in the URL",
            Refused::NotDiscord => {
                "discordWebhook: only discord.com and discordapp.com webhooks are accepted"
            }
            Refused::NotWebhook => {
                "discordWebhook: not a webhook URL; expected https://discord.com/api/webhooks/<id>/<token>"
            }
        }
    }
}

/// The parts of an accepted URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Webhook<'a> {
    /// The host, as written.
    pub host: &'a str,
    /// The webhook's id (digits).
    pub id: &'a str,
    /// The webhook's token: the secret.
    pub token: &'a str,
}

fn digits(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())
}

fn token_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Check `url` (already trimmed): `Ok` with its parts when it is a
/// Discord webhook.
pub fn validate(url: &str) -> Result<Webhook<'_>, Refused> {
    if url.len() > MAX_LEN {
        return Err(Refused::TooLong);
    }
    if !url.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Refused::BadCharacter);
    }
    let rest = url.strip_prefix("https://").ok_or(Refused::NotHttps)?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, rest) = rest.split_at(end);
    if host.contains(['@', ':']) {
        return Err(Refused::UserOrPort);
    }
    if !HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Err(Refused::NotDiscord);
    }
    if rest.contains('#') {
        return Err(Refused::NotWebhook);
    }
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !query.is_empty() {
        let id = query
            .strip_prefix("thread_id=")
            .ok_or(Refused::NotWebhook)?;
        if !digits(id, 20) {
            return Err(Refused::NotWebhook);
        }
    }
    let path = path.strip_prefix("/api/").ok_or(Refused::NotWebhook)?;
    // An API version in front is taken too: /api/v10/webhooks/…
    let path = match path.split_once('/') {
        Some((v, p)) if v.len() > 1 && v.starts_with('v') && digits(&v[1..], 3) => p,
        _ => path,
    };
    let path = path.strip_prefix("webhooks/").ok_or(Refused::NotWebhook)?;
    let (id, token) = path.split_once('/').ok_or(Refused::NotWebhook)?;
    if !digits(id, 20) || !token_ok(token) {
        return Err(Refused::NotWebhook);
    }
    Ok(Webhook { host, id, token })
}

/// The token of a stored URL, for the scrubber; `None` when the URL is
/// not an accepted one.
pub fn token(url: &str) -> Option<&str> {
    validate(url).ok().map(|w| w.token)
}

/// `text` with the URL and its token replaced by `***`: for an error
/// that might carry them (an HTTP library naming the URL it failed on).
pub fn scrub(text: &str, url: &str) -> String {
    let mut out = text.to_string();
    if !url.is_empty() {
        out = out.replace(url, "***");
    }
    if let Some(t) = token(url) {
        out = out.replace(t, "***");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "AbCdEf_123-xyzXYZ0987654321abcdefABCDEF_-0123456789abcdefghijklmnopq";

    fn url(host: &str) -> String {
        format!("https://{host}/api/webhooks/1234567890123456789/{TOKEN}")
    }

    #[test]
    fn discords_own_urls_are_taken() {
        for h in HOSTS {
            let u = url(h);
            let w = validate(&u).unwrap_or_else(|e| panic!("{h}: {e:?}"));
            assert_eq!(w.id, "1234567890123456789");
            assert_eq!(w.token, TOKEN);
            assert_eq!(w.host, *h);
        }
        assert!(validate(&url("Discord.COM")).is_ok());
        let v10 = format!("https://discord.com/api/v10/webhooks/123/{TOKEN}");
        assert!(validate(&v10).is_ok());
        let thread = format!("{}?thread_id=987654321", url("discord.com"));
        assert!(validate(&thread).is_ok());
    }

    #[test]
    fn everything_else_is_refused_with_its_reason() {
        let ok = url("discord.com");
        let cases: Vec<(String, Refused)> = vec![
            (ok.replace("https://", "http://"), Refused::NotHttps),
            (ok.replace("https://", "ftp://"), Refused::NotHttps),
            (ok.replace("https://", ""), Refused::NotHttps),
            (url("discord.com.evil.example"), Refused::NotDiscord),
            (url("evil-discord.com"), Refused::NotDiscord),
            (url("discord.gg"), Refused::NotDiscord),
            (url("example.com"), Refused::NotDiscord),
            (url("discord.com:8443"), Refused::UserOrPort),
            (url("me@discord.com"), Refused::UserOrPort),
            (url("discord.com@evil.example"), Refused::UserOrPort),
            (
                "https://discord.com/api/webhooks/123".into(),
                Refused::NotWebhook,
            ),
            (
                "https://discord.com/api/webhooks/123/".into(),
                Refused::NotWebhook,
            ),
            (
                "https://discord.com/api/webhooks/abc/token".into(),
                Refused::NotWebhook,
            ),
            (
                "https://discord.com/api/webhooks/123/tok/extra".into(),
                Refused::NotWebhook,
            ),
            (
                "https://discord.com/channels/123/456".into(),
                Refused::NotWebhook,
            ),
            (
                "https://discord.com/api/v10/users/@me".into(),
                Refused::NotWebhook,
            ),
            (format!("{ok}?wait=true"), Refused::NotWebhook),
            (format!("{ok}#frag"), Refused::NotWebhook),
            (format!("{ok} "), Refused::BadCharacter),
            (format!("{ok}\n"), Refused::BadCharacter),
            (
                ok.replace("discord.com", "dïscord.com"),
                Refused::BadCharacter,
            ),
            (format!("{ok}{}", "a".repeat(MAX_LEN)), Refused::TooLong),
            (String::new(), Refused::NotHttps),
        ];
        for (u, want) in cases {
            assert_eq!(validate(&u).err(), Some(want), "{u:?}");
            assert!(want.text().starts_with("discordWebhook: "));
        }
    }

    #[test]
    fn the_token_is_found_and_scrubbed() {
        let u = url("discord.com");
        assert_eq!(token(&u), Some(TOKEN));
        assert_eq!(token("https://example.com/x"), None);
        let e = format!("POST {u} failed; token {TOKEN}");
        let s = scrub(&e, &u);
        assert!(!s.contains(TOKEN), "{s}");
        assert_eq!(s, "POST *** failed; token ***");
        assert_eq!(scrub("plain error", ""), "plain error");
    }
}
