//! Concurrency tests for the resolver.
//!
//! The upstream is injected, so these exercise the real coalescing, caching and
//! bounding logic against a fake that counts calls — no Spotify session, no
//! network, deterministic.

use std::sync::atomic::{AtomicUsize, Ordering as O};

use super::*;

fn track(urls: usize) -> Arc<Track> {
    Arc::new(Track {
        key: AudioKey([7u8; 16]),
        format: AudioFileFormat::OGG_VORBIS_320,
        urls: (0..urls).map(|i| format!("https://cdn/{i}")).collect(),
        resolved_at: Instant::now(),
    })
}

/// Counts upstream calls and tracks how many ran at the same time.
#[derive(Default)]
struct Upstream {
    calls: AtomicUsize,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

impl Upstream {
    fn enter(&self) {
        self.calls.fetch_add(1, O::SeqCst);
        let now = self.in_flight.fetch_add(1, O::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, O::SeqCst);
    }
    fn leave(&self) {
        self.in_flight.fetch_sub(1, O::SeqCst);
    }
}

/// Fire `n` concurrent `get`s and collect the outcomes.
fn storm(resolver: Arc<Resolver>, uris: Vec<String>) -> Vec<Result<Arc<Track>, String>> {
    let handles: Vec<_> = uris
        .into_iter()
        .map(|uri| {
            let r = Arc::clone(&resolver);
            std::thread::spawn(move || r.get(&uri))
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

/// The property the whole design rests on: a hundred guests pressing play on
/// the same song must cost exactly one audio-key request.
#[test]
fn concurrent_requests_for_one_track_cost_one_upstream_call() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let resolver = Arc::new(Resolver::spawn(
        move |_uri| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                // Long enough that all 100 callers pile up while it runs.
                tokio::time::sleep(Duration::from_millis(200)).await;
                u.leave();
                Ok(track(3))
            }
        },
        UPSTREAM_CONCURRENCY,
    ));

    let uris = vec!["spotify:track:same".to_string(); 100];
    let results = storm(Arc::clone(&resolver), uris);

    assert_eq!(results.len(), 100);
    assert!(results.iter().all(|r| r.is_ok()), "every caller must be served");
    assert_eq!(
        up.calls.load(O::SeqCst),
        1,
        "100 concurrent requests for one track must coalesce into a single \
         upstream call — anything more is a cache stampede"
    );
    let info = resolver.state.coalesced.load(Ordering::Relaxed);
    assert_eq!(info, 99, "99 callers should have folded into the first");
}

/// Once resolved, further requests must not touch the session at all.
#[test]
fn a_resolved_track_is_served_from_cache() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let resolver = Arc::new(Resolver::spawn(
        move |_uri| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                u.leave();
                Ok(track(1))
            }
        },
        UPSTREAM_CONCURRENCY,
    ));

    assert!(resolver.get("spotify:track:a").is_ok());
    for _ in 0..50 {
        assert!(resolver.get("spotify:track:a").is_ok());
    }
    assert_eq!(up.calls.load(O::SeqCst), 1, "cache must absorb the repeats");
    assert_eq!(resolver.state.hits.load(Ordering::Relaxed), 50);
}

/// Distinct tracks must overlap — that is the fix for the serial worker — but
/// never exceed the bound, because every call shares one librespot session.
#[test]
fn distinct_tracks_run_concurrently_up_to_the_bound() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let limit = 4;
    let resolver = Arc::new(Resolver::spawn(
        move |_uri| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                tokio::time::sleep(Duration::from_millis(120)).await;
                u.leave();
                Ok(track(1))
            }
        },
        limit,
    ));

    let uris: Vec<String> = (0..40).map(|i| format!("spotify:track:{i}")).collect();
    let started = Instant::now();
    let results = storm(Arc::clone(&resolver), uris);
    let elapsed = started.elapsed();

    assert!(results.iter().all(|r| r.is_ok()));
    assert_eq!(up.calls.load(O::SeqCst), 40, "40 distinct tracks, 40 calls");
    assert!(
        up.peak_in_flight.load(O::SeqCst) <= limit,
        "upstream concurrency must never exceed {limit}, saw {}",
        up.peak_in_flight.load(O::SeqCst)
    );
    // Serial would be 40 * 120ms = 4.8s. Bounded-concurrent is ~10 * 120ms.
    assert!(
        elapsed < Duration::from_millis(3_000),
        "40 tracks at concurrency {limit} should take about 1.2s, took {elapsed:?} \
         — that is serial behaviour"
    );
}

/// A mixed, realistic load: lots of people, a handful of popular tracks.
#[test]
fn a_realistic_room_makes_few_upstream_calls() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let resolver = Arc::new(Resolver::spawn(
        move |_uri| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                tokio::time::sleep(Duration::from_millis(80)).await;
                u.leave();
                Ok(track(3))
            }
        },
        UPSTREAM_CONCURRENCY,
    ));

    // 100 guests across 10 tracks.
    let uris: Vec<String> = (0..100)
        .map(|i| format!("spotify:track:{}", i % 10))
        .collect();
    let results = storm(Arc::clone(&resolver), uris);

    assert!(results.iter().all(|r| r.is_ok()), "all 100 served");
    assert_eq!(
        up.calls.load(O::SeqCst),
        10,
        "100 guests over 10 tracks must cost 10 upstream calls, not 100"
    );
}

/// A failure must reach every waiter, and must not poison other tracks or the
/// cache — the next attempt should try again.
#[test]
fn failures_fan_out_and_are_not_cached() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let resolver = Arc::new(Resolver::spawn(
        move |uri: String| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                tokio::time::sleep(Duration::from_millis(60)).await;
                u.leave();
                if uri.ends_with("bad") {
                    Err("no vorbis file for track".to_string())
                } else {
                    Ok(track(1))
                }
            }
        },
        UPSTREAM_CONCURRENCY,
    ));

    let mut uris = vec!["spotify:track:bad".to_string(); 20];
    uris.extend(vec!["spotify:track:good".to_string(); 20]);
    let results = storm(Arc::clone(&resolver), uris);

    let failed = results.iter().filter(|r| r.is_err()).count();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(failed, 20, "every waiter on the bad track gets the error");
    assert_eq!(ok, 20, "a bad track must not take the good one down with it");
    assert_eq!(up.calls.load(O::SeqCst), 2, "one call per distinct track");

    // Not cached: asking again retries rather than replaying the failure.
    assert!(resolver.get("spotify:track:bad").is_err());
    assert_eq!(up.calls.load(O::SeqCst), 3, "the failure was retried");
    // ...while the good one is still cached.
    assert!(resolver.get("spotify:track:good").is_ok());
    assert_eq!(up.calls.load(O::SeqCst), 3, "the success stayed cached");
}

/// A caller that arrives while a resolve is in flight must get the result, not
/// a second call — even if it arrives very late in the window.
#[test]
fn a_late_arrival_still_joins_the_in_flight_resolve() {
    let up = Arc::new(Upstream::default());
    let u = Arc::clone(&up);
    let resolver = Arc::new(Resolver::spawn(
        move |_uri| {
            let u = Arc::clone(&u);
            async move {
                u.enter();
                tokio::time::sleep(Duration::from_millis(300)).await;
                u.leave();
                Ok(track(1))
            }
        },
        UPSTREAM_CONCURRENCY,
    ));

    let first = {
        let r = Arc::clone(&resolver);
        std::thread::spawn(move || r.get("spotify:track:x"))
    };
    std::thread::sleep(Duration::from_millis(150)); // halfway through
    let second = {
        let r = Arc::clone(&resolver);
        std::thread::spawn(move || r.get("spotify:track:x"))
    };

    assert!(first.join().unwrap().is_ok());
    assert!(second.join().unwrap().is_ok());
    assert_eq!(up.calls.load(O::SeqCst), 1);
}

/// The worker dying must surface as an error rather than hanging every guest
/// until the 45s timeout.
#[test]
fn a_dead_worker_fails_fast() {
    let resolver = Resolver::spawn(
        move |_uri| async move { Ok(track(1)) },
        UPSTREAM_CONCURRENCY,
    );
    drop(resolver.tx.clone());
    // Close the channel by replacing the sender with a disconnected one.
    let (tx, rx) = flume::unbounded::<String>();
    drop(rx);
    let dead = Resolver {
        state: Arc::clone(&resolver.state),
        tx,
    };
    let started = Instant::now();
    let err = dead.get("spotify:track:z").unwrap_err();
    assert!(err.contains("worker gone"), "got {err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "must not wait for the resolve timeout"
    );
}

// ----------------------------------------------------------------- token

/// A token that changes on every restart silently locks out every guest, so
/// it must round-trip through disk.
#[test]
fn a_token_is_stable_across_restarts() {
    let dir = std::env::temp_dir().join(format!("myx-token-{}", std::process::id()));
    let path = dir.join("room_token");
    let _ = std::fs::remove_dir_all(&dir);

    let first = load_or_create_token(Some(&path));
    assert!(valid_token(&first), "generated token must be 32 hex chars");
    assert_eq!(
        load_or_create_token(Some(&path)),
        first,
        "a restart must reuse the stored token"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the token is a credential, not configuration");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A corrupt or truncated file must not wedge hosting.
#[test]
fn a_malformed_token_file_is_replaced() {
    let dir = std::env::temp_dir().join(format!("myx-token-bad-{}", std::process::id()));
    let path = dir.join("room_token");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    for junk in ["", "nonsense", "zzzz", &"a".repeat(64)] {
        std::fs::write(&path, junk).unwrap();
        let token = load_or_create_token(Some(&path));
        assert!(valid_token(&token), "replaced {junk:?} with an invalid token");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            token,
            "the replacement must be written back"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no state directory, hosting still works — just without persistence.
#[test]
fn no_state_dir_still_yields_a_token() {
    assert!(valid_token(&load_or_create_token(None)));
}

#[test]
fn valid_token_accepts_only_32_hex_chars() {
    assert!(valid_token(&"a".repeat(32)));
    assert!(valid_token("0123456789abcdef0123456789ABCDEF"));
    assert!(!valid_token(&"a".repeat(31)));
    assert!(!valid_token(&"a".repeat(33)));
    assert!(!valid_token(&"g".repeat(32)));
    assert!(!valid_token(""));
}
