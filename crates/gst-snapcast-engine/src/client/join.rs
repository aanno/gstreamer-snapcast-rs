//! Joining a named stream via the server's JSON-RPC control API.
//!
//! A Snapcast client can't choose its stream in the stream protocol: the server
//! plays its group's stream. To play a given stream (e.g. a DLNA session,
//! `docs/dlna.md`), the client finds its own group in `Server.GetStatus` and sets
//! the group's stream with `Group.SetStream`, as a controller (Snapweb) would. A new
//! client gets a group of its own, so this moves only this client, unless the
//! user has grouped it with others.
//!
//! The control API is newline-delimited JSON-RPC 2.0 over TCP (port 1705 by
//! default), on snapcast-rs servers and the C++ snapserver alike.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// How long one join attempt may take (connect and both calls).
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Attempts after a (re)connect: the client or the stream may show up in the
/// server's status a little later.
const ATTEMPTS: u32 = 20;
/// Pause between attempts.
const RETRY_DELAY: Duration = Duration::from_millis(250);

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

/// What `Server.GetStatus` says about a client: its group and the group's stream.
#[derive(Debug, PartialEq, Eq)]
struct Membership {
    group_id: String,
    stream_id: String,
}

/// Finds the group of `client_id` in a `Server.GetStatus` result.
fn membership(status: &Value, client_id: &str) -> Option<Membership> {
    status["server"]["groups"]
        .as_array()?
        .iter()
        .find(|g| {
            g["clients"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c["id"] == client_id))
        })
        .map(|g| Membership {
            group_id: g["id"].as_str().unwrap_or_default().to_owned(),
            stream_id: g["stream_id"].as_str().unwrap_or_default().to_owned(),
        })
}

/// Whether the server has a stream `stream_id`.
fn has_stream(status: &Value, stream_id: &str) -> bool {
    status["server"]["streams"]
        .as_array()
        .is_some_and(|s| s.iter().any(|s| s["id"] == stream_id))
}

/// A minimal JSON-RPC connection to the control port.
struct Rpc {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Rpc {
    async fn connect(host: &str, port: u16) -> Result<Self, String> {
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("control port {host}:{port}: {e}"))?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    /// Calls `method` and returns its result, skipping notifications in between.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        request.push('\n');
        self.writer
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("{method}: {e}"))?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("{method}: {e}"))?;
            if n == 0 {
                return Err(format!("{method}: connection closed"));
            }
            let Ok(response) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if response["id"] != id {
                continue; // a notification, or not ours
            }
            if let Some(error) = response.get("error") {
                return Err(format!("{method}: {error}"));
            }
            return Ok(response["result"].clone());
        }
    }
}

/// Outcome of one attempt.
#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    Joined,
    AlreadyThere,
    /// Not (yet) possible: the reason, worth a retry.
    Retry(String),
}

async fn attempt(host: &str, port: u16, client_id: &str, stream_id: &str) -> Attempt {
    let mut rpc = match Rpc::connect(host, port).await {
        Ok(rpc) => rpc,
        Err(e) => return Attempt::Retry(e),
    };
    let status = match rpc.call("Server.GetStatus", json!({})).await {
        Ok(status) => status,
        Err(e) => return Attempt::Retry(e),
    };
    let Some(member) = membership(&status, client_id) else {
        return Attempt::Retry(format!("client {client_id} not in the server's status yet"));
    };
    if member.stream_id == stream_id {
        return Attempt::AlreadyThere;
    }
    if !has_stream(&status, stream_id) {
        return Attempt::Retry(format!("the server has no stream '{stream_id}' (yet)"));
    }
    match rpc
        .call(
            "Group.SetStream",
            json!({"id": member.group_id, "stream_id": stream_id}),
        )
        .await
    {
        Ok(_) => Attempt::Joined,
        Err(e) => Attempt::Retry(e),
    }
}

/// Puts client `client_id` on stream `stream_id` via the control API at
/// `host:port`, retrying for a few seconds. Logs the outcome; errors are not
/// fatal (the client then plays what the server assigned).
pub(crate) async fn join_stream(host: &str, port: u16, client_id: &str, stream_id: &str) {
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, attempt(host, port, client_id, stream_id)).await
        {
            Ok(Attempt::Joined) => {
                tracing::info!(client = client_id, stream = stream_id, "Joined stream");
                return;
            }
            Ok(Attempt::AlreadyThere) => {
                tracing::debug!(client = client_id, stream = stream_id, "Already on stream");
                return;
            }
            Ok(Attempt::Retry(reason)) => last = reason,
            Err(_) => last = format!("control port {host}:{port}: timed out"),
        }
        tracing::debug!(reason = %last, "Joining stream, retrying");
        tokio::time::sleep(RETRY_DELAY).await;
    }
    tracing::warn!(client = client_id, stream = stream_id, reason = %last,
        "Cannot join the stream; playing the stream the server assigned");
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
            "streams": [{"id": "default"}, {"id": "radio"}]
        }})
    }

    #[test]
    fn finds_the_group_by_exact_client_id() {
        let s = status();
        assert_eq!(
            membership(&s, "aa:bb"),
            Some(Membership {
                group_id: "g2".into(),
                stream_id: "radio".into()
            })
        );
        assert_eq!(membership(&s, "aa:bb#2").unwrap().group_id, "g1");
        assert_eq!(membership(&s, "aa:bb#3"), None);
        assert!(has_stream(&s, "radio"));
        assert!(!has_stream(&s, "dms-1"));
    }

    #[test]
    fn client_id_appends_instances_other_than_one() {
        assert_eq!(client_id("pi", 1), "pi");
        assert_eq!(client_id("pi", 3), "pi#3");
        let mac = client_id("", 1);
        assert_eq!(mac.len(), 17, "a MAC address: {mac}");
    }
}
