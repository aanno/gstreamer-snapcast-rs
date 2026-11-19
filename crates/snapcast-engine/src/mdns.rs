//! mDNS / DNS-SD for Snapcast (`_snapcast._tcp`), like the C++ snapserver and
//! snapclient.
//!
//! Pure Rust (`mdns-sd`, the crate snapclient-rs uses too): no Avahi/Bonjour
//! library, so the server side stays portable (Android, see CLAUDE.md). Discovery
//! still finds servers that announce themselves via Avahi (C++ snapserver), because
//! it is the same protocol on the wire.

use std::time::{Duration, Instant};

use crate::EngineError;

/// The service type Snapcast servers announce their stream port under.
pub const SERVICE_TYPE: &str = snapcast_proto::DEFAULT_MDNS_SERVICE_TYPE;

fn daemon() -> Result<mdns_sd::ServiceDaemon, EngineError> {
    mdns_sd::ServiceDaemon::new().map_err(|e| EngineError::Mdns(e.to_string()))
}

/// Finds a Snapcast server on the local network: the first that answers within
/// `timeout` (and is announced under the instance `name`, if given), as
/// `(address, stream port)`. IPv4 addresses are preferred (an IPv6 link-local
/// address would need a scope id to connect).
#[cfg(feature = "client")]
pub fn discover(timeout: Duration, name: Option<&str>) -> Result<(String, u16), EngineError> {
    let wanted = name.map(|n| format!("{n}.{SERVICE_TYPE}"));
    let mdns = daemon()?;
    let receiver = mdns
        .browse(SERVICE_TYPE)
        .map_err(|e| EngineError::Mdns(e.to_string()))?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info))
                if wanted.as_deref().is_none_or(|w| info.get_fullname() == w) =>
            {
                let addresses = info.get_addresses();
                let address = addresses
                    .iter()
                    .find(|a| a.is_ipv4())
                    .or_else(|| addresses.iter().next())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_owned());
                tracing::info!(
                    name = info.get_fullname(),
                    %address,
                    port = info.get_port(),
                    "Found Snapcast server via mDNS"
                );
                break Ok((address, info.get_port()));
            }
            Ok(_) => continue,
            Err(_) => {
                let which = name.map(|n| format!(" named '{n}'")).unwrap_or_default();
                break Err(EngineError::Mdns(format!(
                    "no Snapcast server{which} ({SERVICE_TYPE}) found within {timeout:?}"
                )));
            }
        }
    };
    let _ = mdns.stop_browse(SERVICE_TYPE);
    let _ = mdns.shutdown();
    result
}

/// A running mDNS announcement of a server's stream port; withdrawn when dropped.
#[cfg(feature = "server")]
pub(crate) struct Announcement {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

#[cfg(feature = "server")]
impl std::fmt::Debug for Announcement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Announcement")
            .field("fullname", &self.fullname)
            .finish()
    }
}

/// Announces a Snapcast server's stream `port` under the instance `name`.
#[cfg(feature = "server")]
pub(crate) fn announce(name: &str, port: u16) -> Result<Announcement, EngineError> {
    let daemon = daemon()?;
    // The host label only has to be unique; the addresses come from the interfaces.
    let host = format!("snapcast-engine-{}-{port}.local.", std::process::id());
    let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, name, &host, "", port, None)
        .map_err(|e| EngineError::Mdns(e.to_string()))?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_owned();
    daemon
        .register(info)
        .map_err(|e| EngineError::Mdns(e.to_string()))?;
    tracing::info!(%fullname, port, "Announcing Snapcast server via mDNS");
    Ok(Announcement { daemon, fullname })
}

#[cfg(feature = "server")]
impl Drop for Announcement {
    fn drop(&mut self) {
        // Sends a goodbye packet, so clients forget us right away.
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}
