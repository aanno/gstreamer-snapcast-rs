//! Playing a named stream as a session, via the server's JSON-RPC control API.
//!
//! A Snapcast client can't choose its stream in the stream protocol: the server
//! plays its group's stream. To play a given stream (e.g. a DLNA session,
//! `docs/dlna-snapcast-protocol.md`), the client finds its own group in
//! `Server.GetStatus` and sets the group's stream with `Group.SetStream`, as a
//! controller (Snapweb) would. A new client gets a group of its own, so this moves
//! only this client, unless the user has grouped it with others.
//!
//! The control connection then stays open, for the session's other directions:
//!
//! - **Properties in:** the stream's `properties` (playback status, position,
//!   metadata, what it accepts), from `Server.GetStatus` and then from the server's
//!   notifications (`Stream.OnProperties`, `Stream.OnUpdate`, `Server.OnUpdate`).
//! - **Commands out:** `Stream.Control` for the stream (play, pause, setPosition,
//!   ...) and `Client.SetVolume` for this client.
//!
//! The control API is newline-delimited JSON-RPC 2.0 over TCP (port 1705 by
//! default), on snapcast-rs servers and the C++ snapserver alike.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Notify, mpsc, watch};

/// How long one join attempt may take (both calls).
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Join attempts per connection: the client or the stream may show up in the
/// server's status a little later.
const ATTEMPTS: u32 = 20;
/// Pause between join attempts.
const RETRY_DELAY: Duration = Duration::from_millis(250);
/// Pause before reconnecting a lost control connection.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Commands queued while there is no control connection.
const COMMAND_QUEUE: usize = 32;

/// The id the server knows this client by: the host id (empty: the MAC address,
/// as snapcast-client sends it), with `#<instance>` for instances other than 1
/// (`Hello::unique_id` in snapcast-proto, `Hello::getUniqueId` in C++).
pub(crate) fn client_id(host_id: &str, instance: u32) -> String {
    let id = if host_id.is_empty() {
        mac_address::get_mac_address()
            .ok()
            .flatten()
            .map(|mac| mac.to_string().to_lowercase())
            .unwrap_or_else(|| "00:00:00:00:00:00".to_owned())
    } else {
        host_id.to_owned()
    };
    if instance == 1 {
        id
    } else {
        format!("{id}#{instance}")
    }
}

/// A session's stream properties, as the server reports them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionProperties {
    /// `playing`, `paused`, `stopped`, or `None`: not reported.
    pub playback: Option<String>,
    /// Playback position in seconds, if reported.
    pub position_secs: Option<f64>,
    /// Track length in seconds, if reported.
    pub duration_secs: Option<f64>,
    /// Track title, if reported.
    pub title: Option<String>,
    /// The stream accepts `play`.
    pub can_play: bool,
    /// The stream accepts `pause`.
    pub can_pause: bool,
    /// The stream accepts `seek` / `setPosition`.
    pub can_seek: bool,
    /// The whole `properties` object, as JSON text.
    pub json: String,
}

impl SessionProperties {
    /// From a Snapcast stream `properties` object.
    fn from_json(p: &Value) -> Self {
        let flag = |k: &str| p[k].as_bool().unwrap_or(false);
        Self {
            playback: p["playbackStatus"].as_str().map(str::to_owned),
            position_secs: p["position"].as_f64(),
            duration_secs: p["metadata"]["duration"].as_f64(),
            title: p["metadata"]["title"].as_str().map(str::to_owned),
            can_play: flag("canPlay"),
            can_pause: flag("canPause"),
            can_seek: flag("canSeek"),
            json: p.to_string(),
        }
    }
}

/// A request from the application to the server.
#[derive(Debug)]
pub(crate) enum Request {
    /// `Stream.Control` for the session's stream; params as JSON text.
    StreamControl {
        command: String,
        params_json: String,
    },
    /// `Client.SetVolume` for this client.
    SetVolume { percent: u16, muted: bool },
}

/// Where and as whom to play the session.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub stream: String,
}

/// The application's side of a session's control connection.
pub(crate) struct Session {
    pub requests: mpsc::Sender<Request>,
    pub properties: watch::Receiver<Option<SessionProperties>>,
    /// Join again (the stream connection was re-established).
    pub rejoin: Arc<Notify>,
}

/// Starts the control task for `target` on the current runtime. It ends when the
/// returned [`Session`]'s request sender is dropped.
pub(crate) fn start(target: Target) -> (Session, tokio::task::JoinHandle<()>) {
    let (requests, rx) = mpsc::channel(COMMAND_QUEUE);
    let (properties_tx, properties) = watch::channel(None);
    let rejoin = Arc::new(Notify::new());
    let task = tokio::spawn({
        let rejoin = Arc::clone(&rejoin);
        async move { run(target, rx, properties_tx, rejoin).await }
    });
    (
        Session {
            requests,
            properties,
            rejoin,
        },
        task,
    )
}

/// Finds the group of `client_id` in a `Server.GetStatus` result: (group id, its
/// stream id).
fn membership(status: &Value, client_id: &str) -> Option<(String, String)> {
    status["server"]["groups"]
        .as_array()?
        .iter()
        .find(|g| {
            g["clients"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c["id"] == client_id))
        })
        .map(|g| {
            (
                g["id"].as_str().unwrap_or_default().to_owned(),
                g["stream_id"].as_str().unwrap_or_default().to_owned(),
            )
        })
}

/// The `properties` of stream `stream_id` in a `Server.GetStatus` result (or in a
/// `Server.OnUpdate` notification, which carries the same object).
fn stream_properties<'a>(status: &'a Value, stream_id: &str) -> Option<&'a Value> {
    let server = if status["server"].is_object() {
        &status["server"]
    } else {
        status
    };
    server["streams"]
        .as_array()?
        .iter()
        .find(|s| s["id"] == stream_id)
        .map(|s| &s["properties"])
}

/// The new properties of `stream_id` in a notification, if it has any.
fn notified_properties<'a>(message: &'a Value, stream_id: &str) -> Option<&'a Value> {
    let params = &message["params"];
    match message["method"].as_str()? {
        "Stream.OnProperties" if params["id"] == stream_id => Some(&params["properties"]),
        "Stream.OnUpdate" if params["id"] == stream_id => Some(&params["stream"]["properties"]),
        "Server.OnUpdate" => stream_properties(params, stream_id),
        _ => None,
    }
}

/// A JSON-RPC connection to the control port: lines are read by a task of their
/// own (`read_line` isn't cancel-safe, and we wait for lines and requests at once).
struct Rpc {
    lines: mpsc::Receiver<Value>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Rpc {
    async fn connect(host: &str, port: u16) -> Result<Self, String> {
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("control port {host}:{port}: {e}"))?;
        let (reader, writer) = stream.into_split();
        let (tx, lines) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Ok(message) = serde_json::from_str::<Value>(&line)
                    && tx.send(message).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            lines,
            writer,
            next_id: 1,
        })
    }

    /// Sends a request; returns its id.
    async fn send(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        request.push('\n');
        self.writer
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("{method}: {e}"))?;
        Ok(id)
    }

    /// Calls `method` and returns its result. Messages in between are dropped
    /// (only used while joining; the properties are read afresh afterwards).
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.send(method, params).await?;
        loop {
            let message = self
                .lines
                .recv()
                .await
                .ok_or_else(|| format!("{method}: connection closed"))?;
            if message["id"] != id {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("{method}: {error}"));
            }
            return Ok(message["result"].clone());
        }
    }
}

/// One join attempt: `Ok(status)` once the client is on the stream (with the
/// status it was checked in), `Err(reason)` to retry.
async fn attempt(rpc: &mut Rpc, target: &Target) -> Result<Value, String> {
    let status = rpc.call("Server.GetStatus", json!({})).await?;
    let (group, current) = membership(&status, &target.client_id)
        .ok_or_else(|| format!("client {} not in the server's status yet", target.client_id))?;
    if current == target.stream {
        tracing::debug!(client = %target.client_id, stream = %target.stream, "On the stream");
        return Ok(status);
    }
    if stream_properties(&status, &target.stream).is_none() {
        return Err(format!(
            "the server has no stream '{}' (yet)",
            target.stream
        ));
    }
    rpc.call(
        "Group.SetStream",
        json!({"id": group, "stream_id": target.stream}),
    )
    .await?;
    tracing::info!(client = %target.client_id, stream = %target.stream, "Joined stream");
    Ok(status)
}

/// Joins the stream on `rpc`, retrying for a few seconds. Returns the status.
async fn join(rpc: &mut Rpc, target: &Target) -> Result<Value, String> {
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, attempt(rpc, target)).await {
            Ok(Ok(status)) => return Ok(status),
            Ok(Err(reason)) => last = reason,
            Err(_) => last = "timed out".to_owned(),
        }
        tracing::debug!(reason = %last, "Joining stream, retrying");
        tokio::time::sleep(RETRY_DELAY).await;
    }
    Err(last)
}

/// Sends `request` on `rpc` (its response is ignored).
async fn forward(rpc: &mut Rpc, target: &Target, request: Request) -> Result<(), String> {
    match request {
        Request::StreamControl {
            command,
            params_json,
        } => {
            let params = serde_json::from_str::<Value>(&params_json).unwrap_or(Value::Null);
            tracing::info!(stream = %target.stream, %command, %params, "Stream.Control");
            rpc.send(
                "Stream.Control",
                json!({"id": target.stream, "command": command, "params": params}),
            )
            .await?;
        }
        Request::SetVolume { percent, muted } => {
            tracing::info!(client = %target.client_id, percent, muted, "Client.SetVolume");
            rpc.send(
                "Client.SetVolume",
                json!({"id": target.client_id, "volume": {"percent": percent, "muted": muted}}),
            )
            .await?;
        }
    }
    Ok(())
}

/// One control connection: join, then follow the properties and forward requests
/// until the connection fails (`Err`), a rejoin is asked for (`Ok(true)`), or the
/// application is gone (`Ok(false)`).
async fn connection(
    target: &Target,
    requests: &mut mpsc::Receiver<Request>,
    properties: &watch::Sender<Option<SessionProperties>>,
    rejoin: &Notify,
) -> Result<bool, String> {
    let mut rpc = Rpc::connect(&target.host, target.port).await?;
    let status = match join(&mut rpc, target).await {
        Ok(status) => status,
        Err(reason) => {
            tracing::warn!(client = %target.client_id, stream = %target.stream, %reason,
                "Cannot join the stream; playing the stream the server assigned");
            // Stay connected for commands and properties anyway.
            rpc.call("Server.GetStatus", json!({})).await?
        }
    };
    let publish = |p: &Value| {
        let new = SessionProperties::from_json(p);
        properties.send_if_modified(|old| {
            if old.as_ref() == Some(&new) {
                return false;
            }
            tracing::debug!(properties = %new.json, "Session properties");
            *old = Some(new);
            true
        });
    };
    if let Some(p) = stream_properties(&status, &target.stream) {
        publish(p);
    }
    loop {
        tokio::select! {
            message = rpc.lines.recv() => {
                let message = message.ok_or("connection closed")?;
                if let Some(p) = notified_properties(&message, &target.stream) {
                    publish(p);
                }
            }
            request = requests.recv() => {
                let Some(request) = request else { return Ok(false) };
                forward(&mut rpc, target, request).await?;
            }
            () = rejoin.notified() => return Ok(true),
        }
    }
}

/// The control task: (re)connects and joins until the application is gone.
async fn run(
    target: Target,
    mut requests: mpsc::Receiver<Request>,
    properties: watch::Sender<Option<SessionProperties>>,
    rejoin: Arc<Notify>,
) {
    loop {
        match connection(&target, &mut requests, &properties, &rejoin).await {
            Ok(true) => continue,
            Ok(false) => return,
            Err(reason) => {
                tracing::debug!(%reason, "Control connection lost, reconnecting");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Value {
        json!({"server": {
            "groups": [
                {"id": "g1", "stream_id": "default", "clients": [{"id": "aa:bb#2"}]},
                {"id": "g2", "stream_id": "radio", "clients": [{"id": "kitchen"}, {"id": "aa:bb"}]}
            ],
            "streams": [
                {"id": "default", "properties": {"playbackStatus": "playing"}},
                {"id": "radio", "properties": {"playbackStatus": "paused", "position": 12.0,
                  "canSeek": true, "metadata": {"title": "News", "duration": 60.5}}}
            ]
        }})
    }

    #[test]
    fn finds_the_group_by_exact_client_id() {
        let s = status();
        assert_eq!(membership(&s, "aa:bb"), Some(("g2".into(), "radio".into())));
        assert_eq!(membership(&s, "aa:bb#2").unwrap().0, "g1");
        assert_eq!(membership(&s, "aa:bb#3"), None);
        assert!(stream_properties(&s, "radio").is_some());
        assert!(stream_properties(&s, "dms-1").is_none());
    }

    #[test]
    fn reads_session_properties() {
        let s = status();
        let p = SessionProperties::from_json(stream_properties(&s, "radio").unwrap());
        assert_eq!(p.playback.as_deref(), Some("paused"));
        assert_eq!(p.position_secs, Some(12.0));
        assert_eq!(p.duration_secs, Some(60.5));
        assert_eq!(p.title.as_deref(), Some("News"));
        assert!(p.can_seek && !p.can_pause && !p.can_play);
    }

    #[test]
    fn picks_properties_from_notifications() {
        let on_properties = json!({"method": "Stream.OnProperties",
            "params": {"id": "radio", "properties": {"playbackStatus": "stopped"}}});
        assert_eq!(
            notified_properties(&on_properties, "radio").unwrap()["playbackStatus"],
            "stopped"
        );
        assert!(notified_properties(&on_properties, "default").is_none());
        let on_update = json!({"method": "Stream.OnUpdate",
            "params": {"id": "radio", "stream": {"id": "radio", "properties": {"position": 3}}}});
        assert_eq!(
            notified_properties(&on_update, "radio").unwrap()["position"],
            3
        );
        let server = json!({"method": "Server.OnUpdate", "params": status()});
        assert_eq!(
            notified_properties(&server, "radio").unwrap()["position"],
            12.0
        );
        let other = json!({"method": "Client.OnVolumeChanged", "params": {"id": "radio"}});
        assert!(notified_properties(&other, "radio").is_none());
    }

    #[test]
    fn client_id_appends_instances_other_than_one() {
        assert_eq!(client_id("pi", 1), "pi");
        assert_eq!(client_id("pi", 3), "pi#3");
        let mac = client_id("", 1);
        assert_eq!(mac.len(), 17, "a MAC address: {mac}");
    }
}
