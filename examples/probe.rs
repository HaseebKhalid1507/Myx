//! Phase-0 spike: authenticate, bring up the myx Connect device, and *drive*
//! playback — proving myx is a real player, not just a passive endpoint.
//!
//!   cargo run --features streaming --bin myx-probe
//!   cargo run --features streaming --bin myx-probe -- spotify:playlist:37i9dQZF1DXcBWIGoYBM5M
//!
//! Once running, type transport commands + Enter:
//!   play | pause | p (toggle) | next | prev | load <uri> | quit

use std::io::BufRead;

use myx::engine::{self, EngineEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    println!("myx-probe: authenticating…");
    let (tx, rx) = flume::unbounded::<EngineEvent>();
    let engine = engine::run(tx).await?;
    println!("myx-probe: Connect device 'myx' is live.");

    // If a context URI was passed, start playing it immediately.
    if let Some(uri) = std::env::args().nth(1) {
        println!("▶ starting playback: {uri}");
        if let Err(err) = engine.play_context(uri) {
            eprintln!("failed to start playback: {err:#}");
        }
    } else {
        println!("(pass a spotify: URI as an arg to start playback, or select 'myx' in Spotify)");
    }
    println!("commands: play | pause | p | next | prev | load <uri> | quit\n");

    // Read stdin commands on a blocking thread, forward over a channel.
    let (cmd_tx, cmd_rx) = flume::unbounded::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            ev = rx.recv_async() => {
                let Ok(ev) = ev else { break };
                match ev {
                    EngineEvent::TrackChanged { uri } => println!("♫ track changed → {uri}"),
                    EngineEvent::Playing { uri, position_ms } => println!("▶ playing   {uri} @ {position_ms}ms"),
                    EngineEvent::Paused { uri, position_ms } => println!("⏸ paused    {uri} @ {position_ms}ms"),
                    EngineEvent::EndOfTrack { uri } => println!("⏹ end       {uri}"),
                }
            }
            cmd = cmd_rx.recv_async() => {
                let Ok(cmd) = cmd else { break };
                let cmd = cmd.trim();
                let result = match cmd.split_once(' ') {
                    Some(("load", uri)) => engine.play_context(uri.trim().to_string()),
                    _ => match cmd {
                        "play" => engine.play(),
                        "pause" => engine.pause(),
                        "p" | "toggle" => engine.toggle(),
                        "next" | "n" => engine.next(),
                        "prev" | "b" => engine.prev(),
                        "quit" | "q" => break,
                        "" => Ok(()),
                        other => {
                            println!("? unknown command: {other}");
                            Ok(())
                        }
                    },
                };
                if let Err(err) = result {
                    eprintln!("command failed: {err:#}");
                }
            }
        }
    }

    Ok(())
}
