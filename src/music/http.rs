//! The music HTTP API (spec 04, FR-4.5 to FR-4.11): the same plain HTTP/1.1 handling and key
//! check as the webhook, its own listener and keys.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::music::config::MusicConfig;
use crate::music::player::{Player, QueueFull, Track};
use crate::music::source::{self, Source, SourceUrl};
use crate::webhook::http::{KEY_HEADER, read_body, read_head};
use crate::webhook::{ApiKey, ChannelRef, RateLimiter, Request, Response};

/// Cap of a JSON body; tracks travel as uploads, never inside JSON.
const MAX_JSON_BYTES: usize = 4096;
/// A whole request, upload included, must arrive within this time.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The part of the bot the handler needs. `BotApi` is the real implementation, tests use a fake.
pub trait MusicApi {
    fn connected(&self) -> impl Future<Output = bool> + Send;
    fn uptime(&self) -> impl Future<Output = Duration> + Send;
    fn channel_exists(&self, id: u32) -> impl Future<Output = bool> + Send;
    /// IDs of the channels `key` may play into, resolved against the live tree.
    fn allowed_ids(&self, key: &ApiKey) -> impl Future<Output = Vec<u32>> + Send;
    fn resolve(&self, channel: &ChannelRef) -> impl Future<Output = Option<u32>> + Send;
    /// The channel the bot is in.
    fn own_channel(&self) -> impl Future<Output = Option<u32>> + Send;
}

/// `POST /play` with a JSON body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayRequest {
    file: Option<String>,
    url: Option<String>,
    channel: Option<u32>,
    #[serde(default)]
    interrupt: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeRequest {
    volume: u8,
}

/// The body cap for a request: an upload up to the track cap, anything else small.
pub fn body_cap(request: &Request, config: &MusicConfig) -> usize {
    if is_upload(request) {
        config.max_track_bytes()
    } else {
        MAX_JSON_BYTES
    }
}

fn is_upload(request: &Request) -> bool {
    request.header("Content-Type").is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("audio/mpeg") || value.starts_with("audio/mp3")
    })
}

fn authenticated<'a>(request: &Request, config: &'a MusicConfig) -> Option<&'a ApiKey> {
    request
        .header(KEY_HEADER)
        .and_then(|key| config.find_key(key))
}

fn path_and_query(request: &Request) -> (&str, &str) {
    request
        .path
        .split_once('?')
        .unwrap_or((request.path.as_str(), ""))
}

/// Handles one fully read request. Rate limit and body size are checked in [`serve`] before
/// the body is read.
pub async fn handle<A: MusicApi>(
    request: &Request,
    config: &MusicConfig,
    server: &str,
    api: &A,
    player: &Player,
) -> Response {
    let (path, query) = path_and_query(request);
    let method = request.method.as_str();
    if path == "/health" {
        if method != "GET" {
            return Response::error(405, "wrong method");
        }
        let body = json!({
            "connected": api.connected().await,
            "uptime_seconds": api.uptime().await.as_secs(),
            "server": server,
        });
        return Response::ok(200, body.to_string());
    }
    let expected = match path {
        "/play" | "/skip" | "/stop" | "/volume" => "POST",
        "/queue" | "/now-playing" => "GET",
        _ => return Response::error(404, "not found"),
    };
    let Some(key) = authenticated(request, config) else {
        return Response::error(401, "unknown or missing key");
    };
    if method != expected {
        return Response::error(405, "wrong method").audit(Some(key), None);
    }
    let response = match path {
        "/play" => return play(request, query, config, api, player, key).await,
        "/queue" => Response::ok(200, json!(player.view()).to_string()),
        "/now-playing" => Response::ok(
            200,
            json!({ "now_playing": player.now_playing() }).to_string(),
        ),
        "/skip" => Response::ok(200, json!({ "skipped": player.skip() }).to_string()),
        "/stop" => Response::ok(
            200,
            json!({ "stopped": true, "dropped": player.stop() }).to_string(),
        ),
        _ => match serde_json::from_slice::<VolumeRequest>(&request.body) {
            Ok(VolumeRequest { volume }) if volume <= 100 => {
                player.set_volume(volume);
                Response::ok(200, json!({ "volume": volume }).to_string())
            }
            _ => Response::error(400, "volume must be a whole number from 0 to 100"),
        },
    };
    response.audit(Some(key), None)
}

async fn play<A: MusicApi>(
    request: &Request,
    query: &str,
    config: &MusicConfig,
    api: &A,
    player: &Player,
    key: &ApiKey,
) -> Response {
    let audit = |response: Response, channel: Option<u32>| response.audit(Some(key), channel);
    if request.declared_len > body_cap(request, config) {
        return audit(Response::error(413, "request body too large"), None);
    }
    let (source, channel, interrupt) = if is_upload(request) {
        match upload_options(query) {
            Some((channel, interrupt)) => {
                (Source::Upload(request.body.clone()), channel, interrupt)
            }
            None => return audit(Response::error(400, "invalid query parameters"), None),
        }
    } else {
        match serde_json::from_slice::<PlayRequest>(&request.body) {
            Ok(PlayRequest {
                file: Some(file),
                url: None,
                channel,
                interrupt,
            }) => (Source::File(file), channel, interrupt),
            Ok(PlayRequest {
                file: None,
                url: Some(url),
                channel,
                interrupt,
            }) => (Source::Url(url), channel, interrupt),
            _ => {
                return audit(
                    Response::error(400, "send JSON with either \"file\" or \"url\""),
                    None,
                );
            }
        }
    };
    let channel = match channel {
        Some(id) => Some(id),
        None => match &config.channel {
            Some(default) => api.resolve(default).await,
            None => api.own_channel().await,
        },
    };
    let Some(channel) = channel else {
        return audit(Response::error(404, "channel not found"), None);
    };
    if !api.channel_exists(channel).await {
        return audit(Response::error(404, "channel not found"), Some(channel));
    }
    if !api.allowed_ids(key).await.contains(&channel) {
        return audit(
            Response::error(403, "channel not allowed for this key"),
            Some(channel),
        );
    }
    if !api.connected().await {
        return audit(Response::error(503, "not connected"), Some(channel));
    }
    let kind = source.kind();
    // Only the host of a URL is logged (FR-4.18).
    let host = match &source {
        Source::Url(url) => SourceUrl::parse(url, &config.urls).ok().map(|url| url.host),
        _ => None,
    };
    let loaded = match source::load(source, config).await {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::info!(key = %key.label, source = kind, host = ?host, reason = %error.reason(), "music: source refused");
            return audit(
                Response::error(error.status(), &error.reason()),
                Some(channel),
            );
        }
    };
    let title = loaded.title.clone();
    match player.enqueue(Track::new(loaded, kind, channel), interrupt) {
        Ok((id, position)) => {
            tracing::info!(key = %key.label, source = kind, host = ?host, channel, id, position, "music: track queued");
            audit(
                Response::ok(
                    200,
                    json!({ "queued": true, "id": id, "position": position, "title": title })
                        .to_string(),
                ),
                Some(channel),
            )
        }
        Err(QueueFull) => audit(Response::error(409, "queue is full"), Some(channel)),
    }
}

/// `channel` and `interrupt` of an upload's query string.
fn upload_options(query: &str) -> Option<(Option<u32>, bool)> {
    let mut channel = None;
    let mut interrupt = false;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        match pair.split_once('=')? {
            ("channel", id) => channel = Some(id.parse().ok()?),
            ("interrupt", "true" | "1") => interrupt = true,
            ("interrupt", "false" | "0") => interrupt = false,
            _ => return None,
        }
    }
    Some((channel, interrupt))
}

/// Listens and answers one request per connection; requests run concurrently.
pub async fn serve<A: MusicApi + Send + Sync + 'static>(
    listener: TcpListener,
    config: Arc<MusicConfig>,
    server: String,
    api: Arc<A>,
    player: Arc<Player>,
) -> Result<()> {
    let limiter = Arc::new(tokio::sync::Mutex::new(RateLimiter::new(
        config.rate_limit_per_minute,
    )));
    let server: Arc<str> = server.into();
    while let Ok((mut socket, peer)) = listener.accept().await {
        let (config, api, player, limiter, server) = (
            Arc::clone(&config),
            Arc::clone(&api),
            Arc::clone(&player),
            Arc::clone(&limiter),
            Arc::clone(&server),
        );
        tokio::spawn(async move {
            let answered = tokio::time::timeout(
                REQUEST_TIMEOUT,
                answer(
                    &mut socket,
                    &config,
                    &server,
                    api.as_ref(),
                    &player,
                    &limiter,
                ),
            )
            .await;
            let (path, response) = match answered {
                Ok(Ok(answered)) => answered,
                Ok(Err(error)) => {
                    tracing::warn!(peer = %peer, %error, "music: request could not be read");
                    return;
                }
                Err(_) => ("-".to_owned(), Response::error(408, "request timeout")),
            };
            // Only metadata: peer, path, key label, channel, status; never the key or a body.
            tracing::info!(
                peer = %peer,
                path = %path,
                key = response.key.as_deref().unwrap_or("-"),
                channel = response
                    .channel
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                status = response.status,
                "music: request"
            );
            if let Err(error) = socket.write_all(&response.to_bytes()).await {
                tracing::warn!(peer = %peer, %error, "music: answer could not be sent");
            }
            // Drain what the client already sent, so closing does not reset the answer.
            let mut scratch = [0u8; 512];
            let drain = Duration::from_millis(200);
            let _ = tokio::time::timeout(drain, socket.read(&mut scratch)).await;
        });
    }
    Ok(())
}

/// Reads the head, checks key, rate and size before the body is read, then handles it.
async fn answer<A: MusicApi>(
    socket: &mut TcpStream,
    config: &MusicConfig,
    server: &str,
    api: &A,
    player: &Player,
    limiter: &tokio::sync::Mutex<RateLimiter>,
) -> Result<(String, Response)> {
    let head = read_head(socket).await?;
    let path = path_and_query(&head).0.to_owned();
    if path != "/health" {
        let Some(key) = authenticated(&head, config) else {
            return Ok((path, Response::error(401, "unknown or missing key")));
        };
        if !limiter.lock().await.allow(&key.label, Instant::now()) {
            return Ok((
                path,
                Response::error(429, "rate limit reached").audit(Some(key), None),
            ));
        }
        if head.declared_len > body_cap(&head, config) {
            return Ok((
                path,
                Response::error(413, "request body too large").audit(Some(key), None),
            ));
        }
    }
    let cap = body_cap(&head, config);
    let request = read_body(socket, head, cap).await?;
    Ok((path, handle(&request, config, server, api, player).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOBBY: u32 = 1;
    const MUSIC: u32 = 5;

    struct FakeApi {
        connected: bool,
    }

    impl MusicApi for FakeApi {
        async fn connected(&self) -> bool {
            self.connected
        }
        async fn uptime(&self) -> Duration {
            Duration::from_secs(7)
        }
        async fn channel_exists(&self, id: u32) -> bool {
            [0, LOBBY, MUSIC].contains(&id)
        }
        async fn allowed_ids(&self, key: &ApiKey) -> Vec<u32> {
            key.channels
                .iter()
                .filter_map(|channel| match channel {
                    ChannelRef::Id(id) => Some(*id),
                    ChannelRef::Name(name) if name == "Music" => Some(MUSIC),
                    ChannelRef::Name(_) => None,
                })
                .collect()
        }
        async fn resolve(&self, channel: &ChannelRef) -> Option<u32> {
            match channel {
                ChannelRef::Id(id) => Some(*id),
                ChannelRef::Name(name) if name == "Music" => Some(MUSIC),
                ChannelRef::Name(_) => None,
            }
        }
        async fn own_channel(&self) -> Option<u32> {
            Some(LOBBY)
        }
    }

    fn config(media: &std::path::Path) -> MusicConfig {
        toml::from_str(&format!(
            "media_dir = {media:?}\nchannel = \"Music\"\n\
             [[keys]]\nlabel = \"ci\"\nkey = \"secret-ci\"\nchannels = [\"Music\"]\n"
        ))
        .expect("test config")
    }

    fn request(method: &str, path: &str, key: Option<&str>, body: &[u8]) -> Request {
        let mut headers = Vec::new();
        if let Some(key) = key {
            headers.push((KEY_HEADER.to_owned(), key.to_owned()));
        }
        Request {
            method: method.to_owned(),
            path: path.to_owned(),
            headers,
            body: body.to_vec(),
            declared_len: body.len(),
        }
    }

    fn sample() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testenv/audio/speech.mp3"
        ))
        .expect("bundled test sample")
    }

    async fn call(config: &MusicConfig, player: &Player, request: &Request) -> (u16, String) {
        let api = FakeApi { connected: true };
        let response = handle(request, config, "localhost", &api, player).await;
        (response.status, response.json.unwrap_or_default())
    }

    #[tokio::test]
    async fn the_key_is_checked_before_anything_else() -> anyhow::Result<()> {
        let media = tempfile::tempdir()?;
        let config = config(media.path());
        let player = Player::new(&config);
        let body = br#"{"file":"speech.mp3"}"#;
        for key in [None, Some("wrong"), Some("secret-c")] {
            let (status, _) = call(&config, &player, &request("POST", "/play", key, body)).await;
            assert_eq!(status, 401, "{key:?}");
        }
        for (method, path) in [("POST", "/skip"), ("GET", "/queue"), ("POST", "/stop")] {
            let (status, _) = call(&config, &player, &request(method, path, None, b"")).await;
            assert_eq!(status, 401, "{path}");
        }
        let (status, body) = call(&config, &player, &request("GET", "/health", None, b"")).await;
        assert_eq!(status, 200);
        assert!(body.contains("\"connected\":true"), "{body}");
        let (status, _) = call(
            &config,
            &player,
            &request("GET", "/play", Some("secret-ci"), b""),
        )
        .await;
        assert_eq!(status, 405);
        let (status, _) = call(
            &config,
            &player,
            &request("GET", "/nope", Some("secret-ci"), b""),
        )
        .await;
        assert_eq!(status, 404);
        Ok(())
    }

    #[tokio::test]
    async fn play_queue_skip_stop_and_volume() -> anyhow::Result<()> {
        let media = tempfile::tempdir()?;
        std::fs::write(media.path().join("speech.mp3"), sample())?;
        let config = config(media.path());
        let player = Player::new(&config);
        let key = Some("secret-ci");

        let play = request("POST", "/play", key, br#"{"file":"speech.mp3"}"#);
        let (status, body) = call(&config, &player, &play).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"position\":0"), "{body}");
        let (status, body) = call(&config, &player, &play).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"position\":1"), "{body}");

        let upload = Request {
            headers: vec![
                (KEY_HEADER.to_owned(), "secret-ci".to_owned()),
                ("Content-Type".to_owned(), "audio/mpeg".to_owned()),
            ],
            ..request("POST", "/play?channel=5&interrupt=true", key, &sample())
        };
        let (status, body) = call(&config, &player, &upload).await;
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"position\":0"), "{body}");

        let (status, body) = call(&config, &player, &request("GET", "/queue", key, b"")).await;
        assert_eq!(status, 200);
        let view: serde_json::Value = serde_json::from_str(&body)?;
        assert_eq!(view["queue"].as_array().map(Vec::len), Some(3));
        assert_eq!(view["queue"][0]["source"], "upload");
        assert_eq!(view["queue"][1]["channel"], MUSIC);
        assert_eq!(view["volume"], 60);

        let (_, body) = call(&config, &player, &request("POST", "/skip", key, b"")).await;
        assert_eq!(body, r#"{"skipped":false}"#, "nothing plays in this test");
        let (status, body) = call(
            &config,
            &player,
            &request("POST", "/volume", key, br#"{"volume":30}"#),
        )
        .await;
        assert_eq!((status, body.as_str()), (200, r#"{"volume":30}"#));
        let (status, _) = call(
            &config,
            &player,
            &request("POST", "/volume", key, br#"{"volume":101}"#),
        )
        .await;
        assert_eq!(status, 400);
        let (_, body) = call(&config, &player, &request("POST", "/stop", key, b"")).await;
        assert_eq!(body, r#"{"dropped":3,"stopped":true}"#);
        let (_, body) = call(&config, &player, &request("GET", "/now-playing", key, b"")).await;
        assert_eq!(body, r#"{"now_playing":null}"#);
        Ok(())
    }

    #[tokio::test]
    async fn sources_channels_and_limits_are_checked() -> anyhow::Result<()> {
        let outside = tempfile::tempdir()?;
        let media = outside.path().join("media");
        std::fs::create_dir(&media)?;
        std::fs::write(outside.path().join("bot.toml"), b"secret")?;
        std::fs::write(media.join("text.mp3"), b"no audio here")?;
        std::fs::write(media.join("speech.mp3"), sample())?;
        let mut config = config(&media);
        config.max_queue = 1;
        let player = Player::new(&config);
        let key = Some("secret-ci");
        let play = |body: &str| request("POST", "/play", key, body.as_bytes());

        for (body, expected) in [
            (r#"{"file":"../bot.toml"}"#, 403),
            (r#"{"file":"missing.mp3"}"#, 404),
            (r#"{"file":"text.mp3"}"#, 400),
            (r#"{"url":"https://example.com/a.mp3"}"#, 403),
            (
                r#"{"file":"speech.mp3","url":"https://example.com/a.mp3"}"#,
                400,
            ),
            (r#"{"file":"speech.mp3","channel":1}"#, 403),
            (r#"{"file":"speech.mp3","channel":99}"#, 404),
            (r#"{"song":"speech.mp3"}"#, 400),
        ] {
            let (status, answer) = call(&config, &player, &play(body)).await;
            assert_eq!(status, expected, "{body}: {answer}");
        }
        assert_eq!(player.view().queue.len(), 0, "nothing refused was queued");

        let mut short = config.clone();
        short.max_track_secs = 1;
        let (status, _) = call(&short, &player, &play(r#"{"file":"speech.mp3"}"#)).await;
        assert_eq!(status, 422);
        let mut small = config.clone();
        small.max_track_mb = 0;
        let (status, _) = call(&small, &player, &play(r#"{"file":"speech.mp3"}"#)).await;
        assert_eq!(status, 413);

        let ok = play(r#"{"file":"speech.mp3"}"#);
        assert_eq!(call(&config, &player, &ok).await.0, 200);
        assert_eq!(
            call(&config, &player, &ok).await.0,
            409,
            "queue of one is full"
        );

        let offline = FakeApi { connected: false };
        let response = handle(&ok, &config, "localhost", &offline, &player).await;
        assert_eq!(response.status, 503);
        Ok(())
    }

    #[test]
    fn upload_options_are_parsed_strictly() {
        assert_eq!(upload_options(""), Some((None, false)));
        assert_eq!(
            upload_options("channel=3&interrupt=1"),
            Some((Some(3), true))
        );
        assert_eq!(upload_options("channel=x"), None);
        assert_eq!(upload_options("volume=3"), None);
    }
}
