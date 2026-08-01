//! A headless room guest, with no Spotify account of any kind.
//!
//! This holds no credentials: no librespot session, no Web API token, no
//! client id. Everything it plays comes from the host's room over plain HTTP.
//! It exists to prove the guest half of the room works end to end against a
//! real host, which the unit tests can only fake.
//!
//!   cargo run --example roomguest -- <host:port> <token> <spotify:track:...>

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use myx::room::guest_probe::{self, GuestReport};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, token, uri] = args.as_slice() else {
        bail!("usage: roomguest <host:port> <token> <spotify:track:...>");
    };

    // Prove there is nothing to authenticate with before we start.
    for var in ["MYX_CLIENT_ID", "SPOTIFY_CLIENT_ID"] {
        if std::env::var_os(var).is_some() {
            bail!("{var} is set — this probe must run with no credentials");
        }
    }
    println!("guest: no client id, no librespot session, no web api token");
    println!("guest: joining {url}");

    let started = Instant::now();
    let GuestReport {
        joined,
        resolved_format,
        cdn_urls,
        downloaded_bytes,
        ogg_bytes,
        duration,
        sample_rate,
        channels,
        peak,
        played_ms,
    } = guest_probe::run(url, token, uri).context("guest probe")?;

    println!("guest: joined            {joined}");
    println!("guest: format            {resolved_format}");
    println!("guest: cdn urls          {cdn_urls}");
    println!("guest: downloaded        {downloaded_bytes} bytes (encrypted)");
    println!("guest: decrypted ogg     {ogg_bytes} bytes");
    println!("guest: decoded           {duration:?}, {sample_rate} Hz, {channels}ch");
    println!("guest: peak amplitude    {peak:.4}");
    println!("guest: sink advanced to  {played_ms} ms");
    println!("guest: total elapsed     {:?}", started.elapsed());

    if peak <= 0.001 {
        bail!("decoded audio is silence — peak {peak}");
    }
    if played_ms == 0 {
        bail!("the sink never advanced");
    }
    if duration < Duration::from_secs(30) {
        bail!("decoded stream is suspiciously short: {duration:?}");
    }
    println!("\nguest: OK — real Spotify audio played with no Spotify account");
    Ok(())
}
