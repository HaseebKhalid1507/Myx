//! OAuth 2.0 PKCE flow for Spotify.
//!
//! Auth helpers adapted from aome510/spotify-player (`auth.rs`, MIT, © 2021 Thang
//! Pham). We authenticate with librespot's public desktop client id, spin a
//! tiny localhost callback server for the redirect, and exchange the code for an
//! access token that librespot turns into `Credentials`.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use anyhow::{Context, Result};
use base64::Engine as _;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use sha2::{Digest as _, Sha256};

/// librespot's public desktop client id — works for the Connect/streaming flow.
pub const SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

pub const REDIRECT_URI: &str = "http://127.0.0.1:8989/login";
const REDIRECT_ADDR: &str = "127.0.0.1:8989";

pub const OAUTH_SCOPES: &[&str] = &[
    "streaming",
    "app-remote-control",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-top-read",
    "user-read-recently-played",
];

/// Get credentials: prefer the librespot cache, else run an interactive OAuth flow.
pub fn get_creds(cache: &Cache) -> Result<Credentials> {
    if let Some(creds) = cache.credentials() {
        return Ok(creds);
    }
    let token = get_oauth_access_token()?;
    Ok(Credentials::with_access_token(token))
}

fn get_oauth_access_token() -> Result<String> {
    let pkce = Pkce::new_random();
    let state = random_url_safe(16);
    let auth_url = build_authorize_url(&pkce.challenge, &state)?;

    let _ = open::that_in_background(&auth_url);
    println!("Browse to authorize myx:\n{auth_url}\n");

    let addr: SocketAddr = REDIRECT_ADDR.parse().context("parse redirect addr")?;
    let code = listen_for_auth_code(addr)?;
    exchange_code_for_token(&code, &pkce.verifier)
}

fn build_authorize_url(challenge: &str, state: &str) -> Result<String> {
    let scope = OAUTH_SCOPES.join(" ");
    let params = [
        ("response_type", "code"),
        ("client_id", SPOTIFY_CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", scope.as_str()),
        ("code_challenge_method", "S256"),
        ("code_challenge", challenge),
        ("state", state),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("{SPOTIFY_AUTHORIZE_URL}?{query}"))
}

fn listen_for_auth_code(addr: SocketAddr) -> Result<String> {
    let listener =
        TcpListener::bind(addr).with_context(|| format!("bind OAuth callback server to {addr}"))?;

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut request_line = String::new();
        if BufReader::new(&stream).read_line(&mut request_line).is_err() {
            continue;
        }
        // "GET /login?code=...&state=... HTTP/1.1"
        let target = request_line.split_whitespace().nth(1).unwrap_or_default();
        if let Some(code) = code_from_redirect(target) {
            respond(
                &mut stream,
                "200 OK",
                "myx authenticated. You can close this tab.",
            );
            return Ok(code);
        }
        respond(&mut stream, "404 Not Found", "");
    }
    anyhow::bail!("OAuth callback server stopped before receiving a code");
}

fn code_from_redirect(target: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "code").then(|| v.to_string())
    })
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{body}",
        body.len()
    );
}

fn exchange_code_for_token(code: &str, verifier: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
    }

    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", SPOTIFY_CLIENT_ID),
        ("code_verifier", verifier),
    ];

    // reqwest::blocking spins its own runtime; run it off any async thread.
    // Build the x-www-form-urlencoded body by hand so we don't need reqwest's
    // form feature (default features are disabled to keep the tree lean).
    let body = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");

    std::thread::scope(|s| {
        s.spawn(|| {
            let token = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
                .post(SPOTIFY_TOKEN_URL)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .context("send token exchange request")?
                .error_for_status()
                .context("token exchange failed")?
                .json::<TokenResponse>()
                .context("parse token response")?;
            Ok(token.access_token)
        })
        .join()
        .map_err(|_| anyhow::anyhow!("token exchange thread panicked"))?
    })
}

struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    fn new_random() -> Self {
        let verifier = random_url_safe(32);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

fn random_url_safe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::fill(bytes.as_mut_slice());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Minimal percent-encoding for query values (space, and reserved chars).
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
