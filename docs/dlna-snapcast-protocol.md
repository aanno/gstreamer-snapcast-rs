# Snapcast sessions in DLNA/UPnP AV (protocol)

Status: **PoC** (2026-11-27). Implemented in the forks of dms (media server) and
gmrender-resurrect (renderer), with `snapsrc` and `snapsink` from this repo. The
negotiation was tested with BubbleUPnP (DLNA test 10). Playback control was tested in
the devcontainer with a controller's SOAP requests. Design,
decisions and history: [dlna.md](dlna.md). Tests: [dlna-tests.md](dlna-tests.md).

This document is for anyone who wants to add the feature to another media server or
renderer.

## In short

A media server can offer **synchronized multiroom playback** of an item: instead of
the media, a renderer gets the address of a **Snapcast session**, and every renderer
that plays the same item joins the same session and plays in sync. The offer is made
by **HTTP content negotiation on the item's ordinary resource URL**:

```text
renderer ──HEAD /res?path=song.flac   Accept: audio/x-snapcast-tcp──▶ media server
         ◀─200   Content-Type: audio/x-snapcast-tcp─────────────────
renderer ──GET  /res?path=song.flac   Accept: audio/x-snapcast-tcp──▶ media server
         ◀─200   Content-Type: audio/x-snapcast-tcp─────────────────
                 snapcast-tcp://192.168.1.2:1704/dms-3f9a1c?control=1705
renderer ──Snapcast client: connect to 1704, join stream dms-3f9a1c (JSON-RPC on 1705)
```

| Party | Changes |
|---|---|
| Media server | Answers the negotiation; runs (or feeds) the Snapcast server and the session |
| Renderer | Asks before playing an `http://` URI; plays a `snapcast-tcp://` URI with a Snapcast client |
| **Control point** | **None.** It browses, picks a resource and sends `SetAVTransportURI`/`Play` as always |

## Why content negotiation

The obvious ways to offer a second kind of stream don't survive real control points.
BubbleUPnP was tested (DLNA tests 1, 7, 8, 9):

- A resource with its own **protocol** (`snapcast:*:…:*`) is never sent to the renderer.
- An `http-get` resource with an **unknown MIME type** (`audio/x-snapcast`) is never
  sent either, even when the renderer lists that type in `SinkProtocolInfo`.
- The fourth protocolInfo field is **rewritten** by the control point
  (`DLNA.ORG_FLAGS=…;DLNA.ORG_OP=01`), and `CurrentURIMetaData` holds only the chosen
  `<res>`, so nothing in the DIDL-Lite can mark a session.

So the DIDL-Lite stays exactly as it is, and the renderer asks the server. A renderer
or server that doesn't know the feature sees ordinary HTTP.

## 1. The listing

Unchanged. The item has its usual `<res>` elements, e.g.

```xml
<res protocolInfo="http-get:*:audio/flac:DLNA.ORG_OP=01;…" duration="0:03:12.000">
  http://192.168.1.2:1338/res?path=song.flac</res>
```

A media server doesn't advertise the feature anywhere else either. The renderer finds
out per URL.

## 2. The negotiation (HTTP)

### Transports and their MIME types

A Snapcast client can reach a server over more than one wire protocol. Each such
**transport** has a URI scheme for the session and a MIME type that names it in the
negotiation:

| Transport | URI scheme | MIME type | Status |
|---|---|---|---|
| Snapcast's binary protocol over TCP (port 1704) | `snapcast-tcp://` | `audio/x-snapcast-tcp` | In use |
| The same over WebSocket (C++ snapserver: `/stream` on port 1780) | `snapcast-ws://` | `audio/x-snapcast-ws` | Reserved: snapcast-rs has no WebSocket audio yet (L11) |

Parameters may be added to the MIME types later; both sides must ignore unknown ones.

**Choosing a transport is ordinary HTTP content negotiation**, so DLNA itself
needs nothing new:

- The renderer lists every transport it can play in `Accept`, with q values for its
  preference, e.g. `Accept: audio/x-snapcast-tcp, audio/x-snapcast-ws;q=0.9`.
- The media server offers one session URI per transport its Snapcast server serves.
  It answers with the transport the renderer accepts with the highest q; on a tie, it
  uses its own order. Its `Content-Type` names that transport, and the URI uses its
  scheme.
- No common transport: the media, as for any other request.

**Where the knowledge comes from:** the GStreamer plugins, so that no list is
maintained by hand.

- **Renderer:** a transport is playable if an installed GStreamer source handles its
  URI scheme (`gst_uri_protocol_is_supported(GST_URI_SRC, "snapcast-tcp")`).
  `snapsrc` declares the schemes it plays as its URI handler's protocols: today
  `snapcast-tcp`; `snapcast-ws` once snapcast-rs's client can do it. gmrender builds
  its `Accept` header from that. If there's none, it doesn't negotiate at all.
- **Media server:** it needs the session URIs its Snapcast server serves. In the PoC,
  they're given to dms by hand (`-snapcastStream`, once per transport). Later,
  `snapsink` (or the daemon) should publish them, e.g. as a read-only property with
  one URI template per transport its server listens on (`snapcast-tcp://{host}:1704/<stream>?control=1705`;
  with WebSocket audio, also `snapcast-ws://{host}:1780/<stream>`). A server that
  serves both transports to the same stream keeps renderers in sync either way.

### Renderer

Before playing an `http://` URI it got from `SetAVTransportURI` or
`SetNextAVTransportURI`:

1. Send `HEAD <url>` with an `Accept` header listing the playable transports (above).
2. If the response is `200` and its `Content-Type` is one of them (parameters
   allowed), send `GET <url>` with the same header, and read the body's first line.
3. If that line is a URI with that transport's scheme (section 3), play the session
   instead of the URL. In anything else (another status, another type, a timeout, a
   bad body), play the URL as usual.

Notes:

- Only `http://` URLs are asked. `https://` can be added the same way.
- **Timeouts:** a few seconds at most (gmrender: 3 s for connecting and for each
  read). The negotiation runs before playback starts.
- A server that doesn't know the feature answers the `HEAD` with the media's type. That
  costs one small extra request per track.
- A renderer **may** skip the `HEAD` and send only the `GET`. If the response isn't a
  session, it must then close the connection after the header, or use the response as
  the media.
- The renderer keeps reporting the control point's URL (`AVTransportURI`,
  `CurrentTrackURI`), not the session URI. The control point never sees the session.
- The feature should be switchable (gmrender: `--snapcast-sessions`), so that a room
  can still play an item on its own.

### Media server

For a request for an item's resource:

- **Offer a session** only if the item can be played as one (e.g. audio), sessions are
  enabled, and the `Accept` header names one of the server's transports explicitly with
  `q` above 0. Never because of `*/*` or `audio/*`.
  - Respond `200` with the chosen transport's MIME type as `Content-Type`, plus
    `Vary: Accept` and `Cache-Control: no-store`.
  - For `GET`, the body is the session URI as one line of UTF-8, ending with `\n`.
    For `HEAD`, there's no body.
  - The `GET` is the renderer's **request to open the session**, so the server
    creates it (section 4) before it answers. A `HEAD` doesn't open anything.
- **Otherwise** answer as before (the media, with ranges, transcoding and so on).
  Requests without the header, from control points probing the URL and from renderers
  that don't know Snapcast, keep getting the media.

## 3. The URI

```text
snapcast-tcp://<host>[:<port>]/<stream>[?control=<port>]
```

(`snapcast-ws://` will follow the same pattern, with its own default port; it's
specified when snapcast-rs implements it.)

| Part | Default | Meaning |
|---|---|---|
| `host` | (required in answers) | The Snapcast server: name, IPv4 address or `[IPv6]` |
| `port` | `1704` | Its stream port (TCP) |
| `stream` | (required in answers) | The Snapcast stream id of the session, percent-encoded |
| `control` | `port + 1` | Its JSON-RPC control port (TCP), used to join the stream |

- The **host** must be an address the renderer can reach the Snapcast server at,
  whatever address it used for the media server. dms uses the local address of the
  HTTP request, and the interface's IPv4 address if the request came over IPv6, since
  a Snapcast server may listen on IPv4 only.
- The URI carries no audio format: the Snapcast stream announces it to each client (its
  codec header). It carries no volume either: each room keeps its own (section 5).
- Unknown query parameters must be ignored.
- `snapsrc` (GStreamer) handles this scheme, so a GStreamer renderer can hand the URI
  to `playbin` as it is. It also accepts an empty host (`snapcast-tcp:///stream`) to
  find a server via mDNS. Answers from a media server always name the host.

## 4. Sessions

- **A session is one Snapcast stream.** The media server names it per item (dms:
  `dms-<hash of the item>`), so every renderer that gets the same item joins the same
  session. Two independent playbacks of one item aren't possible, and aren't needed.
- **Joining:** the Snapcast stream protocol can't choose a stream; the server plays the
  client's group's stream. So after connecting, the renderer's Snapcast client looks up
  its own group in `Server.GetStatus` (by its client id) and calls
  `Group.SetStream { id: <group>, stream_id: <stream> }` on the control port. It
  retries for a few seconds (the stream may still be starting), and does it again
  after every reconnect. A new client has a group of its own, so this moves only that
  client, unless the user has grouped it with others.
- **Opening and closing:** the media server opens the session on the renderer's `GET`,
  and closes it when no client has listened to it for a while.

### Playback control

Built on the renderer's side (PoC step 3): gmrender with `snapsrc`, and the stand-in
sender `make dlna-session`. A media server's own feeder comes in step 4.

| UPnP action on a renderer | Effect |
|---|---|
| `SetAVTransportURI` | Negotiation; the renderer joins the session (after every reconnect too) |
| `Play` | `Stream.Control play` on the session (nothing happens if it's already playing) |
| `Pause` | `Stream.Control pause`: **pauses every room** |
| `Seek` | `Stream.Control setPosition`: **seeks every room** |
| `Stop` | Only this renderer leaves the session; the others play on |
| `GetPositionInfo`, `GetTransportInfo` | From the session's stream properties (`Stream.OnProperties`); a pause or resume elsewhere changes the renderer's transport state |
| End of the track | The sender reports `playbackStatus: stopped`; renderers treat it as the end of the stream, so control points move on |
| RenderingControl volume, mute | This room's Snapcast client volume (`Client.SetVolume`), as the control point's 0..100 level |

Pause and seek take effect after up to the Snapcast buffer (`buffer-ms`, default
1 s): clients play out what they already hold.

## 5. What a media server needs

1. The negotiation of section 2, on the existing resource handler, with one session
   URI per transport its Snapcast server serves.
2. A Snapcast server reachable by the renderers, and a way to feed the item into a
   **stream of its own** (one name per item, so that renderers of the same item meet),
   opened on the renderer's `GET` and closed when idle. In the PoC:
   - The Snapcast server is `gst-snapcast-daemon`, shared by all sessions; `snapsink`
     starts it by itself.
   - dms runs one helper process per session (`session-sender`, GStreamer with
     `snapsink`): `-snapcastStream 'snapcast-tcp://{host}:1704/{stream}?control=1705'
     -snapcastFeeder 'session-sender --mode shared --idle-exit 600'`.
3. Handle `Stream.Control` (`play`, `pause`, `playPause`, `stop`, `setPosition`
   `{"position": s}`, `seek` `{"offset": s}`), and publish the stream's
   `playbackStatus` (`stopped` at the end of the track), `position` (seconds, about
   once a second) and `metadata.duration`. With `snapsink` that's
   handling its `snapcast-stream-control` bus message and driving the pipeline. It
   publishes the properties itself. See `snapsink`'s example `session-sender`.

## 6. What a renderer needs

1. The negotiation of section 2, before an `http://` URI goes to the player, asking
   for the transports its player can play.
2. A Snapcast client that plays a `snapcast-tcp://` URI and joins its stream
   (section 4). With GStreamer, that's `snapsrc` (the URI handler picks it by itself).
3. Pause, Seek and position go to the session instead of the local player (the
   player keeps running), and the volume goes to its Snapcast client. With
   `snapsrc`, that's its `snapcast-session` bus message and its action signals
   `stream-control` and `set-server-volume`.

## 7. Security

The Snapcast control port has no authentication: anyone who reaches it can control the
server. The PoC is meant for a home network, like DLNA itself.

## Implementations

| Part | Where | What |
|---|---|---|
| Media server | `dlna/dms` (fork, `dlna/dms/snapcast.go`, `snapcast_feeder.go`) | Negotiation on `/res`, transport by `Accept` q; a stream per item (`{stream}`) fed by a `SessionFeeder`; flags `-snapcastStream` (repeatable, one per transport), `-snapcastFeeder` |
| Renderer | `dlna/gmrender-resurrect` (fork, `src/snapcast_session.c`) | Negotiation before `set_uri`/`set_next_uri`, `Accept` from the GStreamer registry; flag `--snapcast-sessions` |
| Snapcast client | `snapsrc` (`crates/gst-plugin-snapsrc/src/snapsrc/uri.rs`, `imp.rs` "Sessions") | `snapcast-tcp://` URI handler; properties `stream`, `control-port`; message `snapcast-session`; signals `stream-control`, `set-server-volume` |
| Session control | `gst-snapcast-engine` (`src/client/session.rs`) | Join (`Server.GetStatus`, `Group.SetStream`) after each (re)connect; properties in, `Stream.Control`/`Client.SetVolume` out |
| Sender | `snapsink`, example `session-sender` (dms's helper; `make dlna-session` by hand) | Position, duration, `stopped`; obeys `Stream.Control`; `--paused`, `--idle-exit`, `Session ready` line |
