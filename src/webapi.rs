//! Web API authentication — a dedicated OAuth 2.0 PKCE flow using *your own*
//! Spotify app's client id, so metadata/library calls get their own rate-limit
//! bucket instead of fighting over librespot's saturated shared client.
//!
//! Separate from the librespot streaming session: this token only talks to
//! `api.spotify.com`. Cached to `~/.cache/myx/webapi.json` with its refresh
//! token and auto-refreshed before expiry.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const REDIRECT_URI: &str = "http://127.0.0.1:8989/login";
const REDIRECT_ADDR: &str = "127.0.0.1:8989";

const SCOPES: &[&str] = &[
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-follow-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-top-read",
    "user-read-recently-played",
];

/// A short fingerprint of the granted scopes, so a scope change forces re-auth.
fn scopes_tag() -> String {
    SCOPES.join(",")
}

/// Resolve the Spotify app client id: `MYX_CLIENT_ID` env var, else
/// `~/.config/myx/client_id`. No default is bundled — every user brings their
/// own app (create one free at the Spotify developer dashboard).
fn resolve_client_id() -> Result<String> {
    if let Ok(id) = std::env::var("MYX_CLIENT_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    if let Some(home) = crate::home_dir() {
        let path = home.join(".config/myx/client_id");
        if let Ok(s) = std::fs::read_to_string(&path) {
            let id = s.trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
        }
    }
    bail!(
        "No Spotify client id found.\n\
         Create a free app at https://developer.spotify.com/dashboard\n\
         (add redirect URI http://127.0.0.1:8989/login), then either:\n\
         \x20 export MYX_CLIENT_ID=<your-client-id>\n\
         \x20 or write it to ~/.config/myx/client_id"
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
    #[serde(default)]
    scopes: String,
}

/// Holds a live Web API access token and can refresh it on demand.
pub struct WebApi {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
}

impl WebApi {
    /// Load from cache (refreshing if stale), else run an interactive OAuth flow.
    /// Blocking: opens a browser + listens for the redirect. Call off the async
    /// thread (e.g. `spawn_blocking`) and before entering the alternate screen.
    pub fn init() -> Result<Self> {
        let client_id = resolve_client_id()?;

        if let Some(mut w) = Self::from_cache(&client_id) {
            // Only trust a cached token that's either fresh or successfully
            // refreshed; a stale token whose refresh fails falls through to a
            // clean interactive re-auth instead of poisoning the whole session.
            let usable = if w.is_expiring() {
                w.refresh().is_ok()
            } else {
                true
            };
            if usable && !w.access_token.is_empty() {
                return Ok(w);
            }
        }

        let (access_token, refresh_token, expires_in) = authorize(&client_id)?;
        let w = WebApi {
            client_id,
            access_token,
            refresh_token,
            expires_at: now() + expires_in,
        };
        w.save();
        Ok(w)
    }

    /// Return a currently-valid access token, refreshing if it's about to expire.
    pub fn valid_token(&mut self) -> Result<String> {
        if self.is_expiring() {
            self.refresh().context("refresh web token")?;
        }
        Ok(self.access_token.clone())
    }

    /// Clone the current token without performing network I/O. The main thread
    /// refreshes during initialization; background tasks use this to avoid one
    /// slow refresh holding the shared mutex and stalling every API worker.
    pub fn cached_token(&self) -> String {
        self.access_token.clone()
    }

    fn is_expiring(&self) -> bool {
        now() + 60 >= self.expires_at
    }

    fn refresh(&mut self) -> Result<()> {
        let refresh_token = self
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow!("no refresh token cached"))?;
        let resp = post_token_form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", &self.client_id),
        ])?;
        self.access_token = resp
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("refresh: no access_token"))?
            .to_string();
        if let Some(rt) = resp.get("refresh_token").and_then(|v| v.as_str()) {
            self.refresh_token = Some(rt.to_string());
        }
        let expires_in = resp
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);
        self.expires_at = now() + expires_in;
        self.save();
        Ok(())
    }

    fn cache_path() -> Option<PathBuf> {
        Some(crate::home_dir()?.join(".cache/myx/webapi.json"))
    }

    fn from_cache(client_id: &str) -> Option<Self> {
        let path = Self::cache_path()?;
        let data = std::fs::read_to_string(path).ok()?;
        let c: Cached = serde_json::from_str(&data).ok()?;
        if c.client_id != client_id || c.scopes != scopes_tag() {
            return None; // app or scopes changed — re-auth
        }
        Some(WebApi {
            client_id: c.client_id,
            access_token: c.access_token,
            refresh_token: c.refresh_token,
            expires_at: c.expires_at,
        })
    }

    fn save(&self) {
        let Some(path) = Self::cache_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let c = Cached {
            client_id: self.client_id.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            scopes: scopes_tag(),
        };
        if let Ok(json) = serde_json::to_string(&c) {
            // Write atomically, then tighten to 0600 so the refresh token is not
            // world-readable (audit H4).
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
                }
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Full interactive authorization → (access_token, refresh_token, expires_in).
fn authorize(client_id: &str) -> Result<(String, Option<String>, u64)> {
    let verifier = random_url_safe(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let state = random_url_safe(16);

    let scope = SCOPES.join(" ");
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", REDIRECT_URI),
        ("scope", scope.as_str()),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge.as_str()),
        ("state", state.as_str()),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let auth_url = format!("{AUTHORIZE_URL}?{query}");

    let _ = open::that_in_background(&auth_url);
    println!("\nAuthorize myx for the Web API (playlists + metadata):\n{auth_url}\n");

    let code = listen_for_code()?;

    let resp = post_token_form(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", &verifier),
    ])?;

    let access = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no access_token in token response"))?
        .to_string();
    let refresh = resp
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from);
    let expires_in = resp
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    Ok((access, refresh, expires_in))
}

fn listen_for_code() -> Result<String> {
    let listener = TcpListener::bind(REDIRECT_ADDR)
        .with_context(|| format!("bind OAuth callback server to {REDIRECT_ADDR}"))?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        if let Some(code) = code_from_target(target) {
            respond(&mut stream, "myx authorized. You can close this tab.");
            return Ok(code);
        }
        respond(&mut stream, "");
    }
    bail!("callback server stopped before receiving a code")
}

fn code_from_target(target: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "code").then(|| v.to_string())
    })
}

fn respond(stream: &mut TcpStream, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{body}",
        body.len()
    );
}

/// POST an x-www-form-urlencoded body to the token endpoint, return parsed JSON.
fn post_token_form(params: &[(&str, &str)]) -> Result<serde_json::Value> {
    let body = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let json = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default()
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .context("send token request")?
        .error_for_status()
        .context("token request failed")?
        .json::<serde_json::Value>()
        .context("parse token response")?;
    Ok(json)
}

fn random_url_safe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::fill(bytes.as_mut_slice());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
