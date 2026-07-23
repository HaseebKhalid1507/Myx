//! Diagnostic: check the Web API token + /me/playlists response.
//!   cargo run --example libcheck
use myx::engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (tx, _rx) = flume::unbounded();
    let engine = engine::run(tx).await?;

    let token = match engine.web_token().await {
        Ok(t) => {
            println!("web_token OK: len={}, head={}…", t.len(), &t[..t.len().min(10)]);
            t
        }
        Err(e) => {
            println!("web_token ERROR: {e:#}");
            return Ok(());
        }
    };

    // reqwest::blocking must run off the tokio thread.
    let out = tokio::task::spawn_blocking(move || {
        let client = reqwest::blocking::Client::new();
        let mut report = String::new();
        for (label, url) in [
            ("/me", "https://api.spotify.com/v1/me"),
            ("/me/playlists", "https://api.spotify.com/v1/me/playlists?limit=5"),
        ] {
            match client.get(url).bearer_auth(&token).send() {
                Ok(r) => {
                    let status = r.status();
                    let retry = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-")
                        .to_string();
                    let body = r.text().unwrap_or_default();
                    report += &format!(
                        "\n[{label}] status={status} retry-after={retry}s\n{}\n",
                        &body[..body.len().min(300)]
                    );
                }
                Err(e) => report += &format!("\n[{label}] send error: {e}\n"),
            }
        }
        report
    })
    .await
    .unwrap_or_default();

    println!("{out}");
    Ok(())
}
