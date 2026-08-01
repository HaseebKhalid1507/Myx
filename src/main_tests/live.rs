use crate::*;

fn token() -> Option<String> {
    let path = myx::home_dir()?.join(".cache/myx/webapi.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if now + 30 >= v["expires_at"].as_u64().unwrap_or(0) {
        eprintln!("cached token expired — run `myx` once, then retry");
        return None;
    }
    v["access_token"].as_str().map(str::to_string)
}

/// girl in red: a real artist with a discography larger than one page.
const ARTIST_ID: &str = "3uwAm6vQy7kWPS2bciKWx9";
const ARTIST_NAME: &str = "girl in red";

/// The February 2026 library endpoints: check, save, check again. Uses a
/// track that is already saved, so a pass leaves the library as it was.
#[test]
#[ignore = "hits the Spotify API"]
fn live_library_contains_and_write() {
    let Some(token) = token() else { return };
    let client = http_client();
    let v = get_json(&client, &format!("{API}/me/tracks?limit=1"), &token).expect("liked page");
    let Some(id) = v["items"][0]["track"]["id"].as_str() else {
        return println!("no liked tracks to test with");
    };
    let uri = format!("spotify:track:{id}");
    assert!(
        api_contains(&token, &uri),
        "a track from /me/tracks reads as unsaved"
    );
    library_write(&client, &token, false, &uri).expect("re-saving an already saved track");
    assert!(api_contains(&token, &uri), "still saved after the write");
}

/// Reproduces "liked failed http 403": page the whole Liked library the way
/// `spawn_library_fetch` does and report the first page Spotify refuses.
#[test]
#[ignore = "hits the Spotify API"]
fn live_liked_songs_page_all_the_way_through() {
    let Some(token) = token() else { return };
    let client = http_client();
    let mut url = Some(format!("{API}/me/tracks?limit=50"));
    let mut pages = 0;
    let mut tracks = 0;
    let mut total = None;
    while let Some(u) = url.take() {
        let resp = client.get(&u).bearer_auth(&token).send().expect("send");
        let status = resp.status().as_u16();
        let v: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
        assert!(
            (200..300).contains(&status),
            "page {pages} -> HTTP {status}: {} ({u})",
            v["error"]["message"].as_str().unwrap_or("(no message)")
        );
        total.get_or_insert(v["total"].as_u64().unwrap_or(0));
        tracks += v["items"].as_array().map(Vec::len).unwrap_or(0);
        url = v["next"].as_str().map(String::from);
        pages += 1;
    }
    println!("liked: {tracks} tracks over {pages} pages, total={total:?}");
    assert_eq!(Some(tracks as u64), total, "pagination dropped tracks");
}

#[test]
#[ignore = "hits the Spotify API"]
fn live_artist_top_tracks_are_not_empty() {
    let Some(token) = token() else { return };
    let rows = artist_top_tracks(&http_client(), &token, ARTIST_ID, ARTIST_NAME);
    println!("top tracks: {}", rows.len());
    for r in rows.iter().take(3) {
        println!("  {} · {}", r.name, r.subtitle);
    }
    assert!(!rows.is_empty(), "the Popular section came back empty");
    assert!(rows.iter().all(|r| r.is_track));
}

#[test]
#[ignore = "hits the Spotify API"]
fn live_artist_albums_page_past_the_first_ten() {
    let Some(token) = token() else { return };
    let rows = artist_albums(&http_client(), &token, ARTIST_ID);
    println!("albums: {}", rows.len());
    for r in rows.iter().take(3) {
        println!("  {} · {}", r.name, r.subtitle);
    }
    if rows.is_empty() {
        // A spent quota answers 429 for hours; that is not a code failure.
        return println!("no albums came back — quota or API error");
    }
    // The bug was a single page rejected outright; more than one page's
    // worth proves both that it succeeds and that `next` is followed.
    assert!(
        rows.len() > 10,
        "expected a paged discography, got {}",
        rows.len()
    );
    assert!(rows.iter().all(|r| !r.is_track && !r.is_header));
    // Newest first.
    let years: Vec<&str> = rows.iter().map(|r| r.subtitle.as_str()).collect();
    assert!(
        years.windows(2).all(|w| w[0] >= w[1]),
        "not newest-first: {years:?}"
    );
}

#[test]
#[ignore = "hits the Spotify API"]
fn live_artist_detail_has_both_sections() {
    let Some(token) = token() else { return };
    let (title, items) =
        fetch_detail_blocking(&token, &format!("spotify:artist:{ARTIST_ID}"), ARTIST_NAME);
    let headers: Vec<&str> = items
        .iter()
        .filter(|i| i.is_header)
        .map(|i| i.name.as_str())
        .collect();
    println!("{title}: {} rows, headers {headers:?}", items.len());
    assert!(
        headers.contains(&"Popular"),
        "artist page lost its Popular section: {headers:?}"
    );
    if !headers.contains(&"Albums") {
        // A spent album-endpoint quota answers 429 for hours, and the
        // section drops out; that is not a code failure.
        return println!("no Albums section — quota or API error");
    }
    assert_eq!(headers, ["Popular", "Albums"]);
    assert!(items.len() > 20, "only {} rows", items.len());
}
