//! Verify the radio path returns similar tracks.
use myx::engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (tx, _rx) = flume::unbounded();
    let engine = engine::run(tx, 50).await?;
    let session = engine.session();
    let seed = "spotify:track:0VjIjW4GlUZAMYd2vXMi3b";
    match engine::radio_tracks(&session, seed).await {
        Ok(uris) => {
            println!("radio OK: {} tracks", uris.len());
            for u in uris.iter().take(6) {
                println!("  {u}");
            }
        }
        Err(e) => println!("radio ERROR: {e:#}"),
    }
    Ok(())
}
