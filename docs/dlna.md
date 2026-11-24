# DLNA/UPnP with Snapcast streams (design)

Status: **proposed** (2026-11-24), nothing implemented yet. Targets for the proof of
concept: the media server [dms](https://github.com/anacrolix/dms) (Go) and the renderer
[gmrender-resurrect](https://github.com/hzeller/gmrender-resurrect) (C, GStreamer). Both
are used at home and work there. Background: `docs/gstreamer-plugins/dlna-rs-gstreamer-1.md`
(the original idea of a `snapcast://` URI and `snapsrc` in gmrender).

## Goal

Synchronized playback on several DLNA renderers, driven by ordinary DLNA control points,
with as little new protocol as possible:

- The **media server** offers a Snapcast stream as one more resource of an item, next to
  the usual HTTP resource, the same way it offers a transcoded variant.
- The usual **negotiation** decides which resource a renderer gets. A renderer without
  Snapcast support gets the HTTP resource, as it does today.
- Renderers that get the same Snapcast resource play **one shared session**, in sync.

## Decisions so far (2026-11-24, the user)

1. **The media server provides the stream**, not a renderer: it is the media server's
   job to give the renderer a URL for the media, and to serve the media itself when
   needed (transcoding). A Snapcast stream is one more such offer. (A gmrender instance
   with `snapsink` as its output, as a "virtual renderer", was discussed and rejected:
   it works around DLNA instead of using it.)
2. **Negotiation as in DLNA:** the details of the Snapcast stream (codec, sample rate,
   bit depth, channels) go into `protocolInfo`. The HTTP resource is the fallback.
3. **The control point starts and pauses the stream**, as in any DLNA use case.
4. **Pause and seek are global:** they act on the session, for all renderers in it.
5. **A second renderer joins** a running session.
6. **The URI** names host, port and stream, and can carry an initial volume.
7. **dms uses GStreamer for the PoC** (`snapsink` in a `go-gst` pipeline) for
   Snapcast sessions; ffmpeg stays for everything else. Not a clean design, but quick.
8. **The renderer must know the new type and prefer it** over the HTTP resource
   (see "Preferring the Snapcast resource").
9. **Video with the audio split off** (video to a TV, audio as Snapcast to several
   devices) is a valid scenario. It is designed for here but comes after audio.

## Roles

| Role | DLNA | Here | Changes |
|---|---|---|---|
| Media server | ContentDirectory, HTTP | dms | Offers a `snapcast` resource; feeds the session into the Snapcast server |
| Snapcast server | none | `gst-snapcast-daemon` on the media server's machine | Creates sessions on demand for dms (new, below) |
| Renderer | AVTransport, RenderingControl, ConnectionManager | gmrender + `snapsrc` | Accepts `snapcast://` URIs; maps actions to the session |
| Control point | Browse, picks the resource, Play/Pause/Seek | BubbleUPnP, any | None. Must pass an unknown URI scheme through (to verify early) |

## The Snapcast resource

### In the listing (DIDL-Lite)

An item lists several `<res>` elements. dms adds one more:

```xml
<res protocolInfo="http-get:*:audio/flac:*">http://nas:1338/res?path=...</res>
<res protocolInfo="snapcast:*:audio/flac;rate=44100;channels=2;bits=16:DLNA.ORG_OP=00;DLNA.ORG_FLAGS=...">
  snapcast://nas:1704/dms-3f9a1c?control=1705</res>
```

### protocolInfo

`<protocol>:<network>:<contentFormat>:<additionalInfo>`, as in the UPnP
ConnectionManager:

- **protocol** `snapcast`: the renderer connects to a Snapcast server. It is not a file
  to download, so it is not `http-get`.
- **contentFormat** is the codec on the Snapcast wire, with the stream's format as MIME
  parameters. DLNA already does this for LPCM (`audio/L16;rate=44100;channels=2`):
  - `audio/L16;rate=…;channels=…` for codec `pcm` (16 bit; `audio/L24` for 24 bit),
  - `audio/flac;rate=…;channels=…;bits=…` for codec `flac`,
  - `audio/opus;…` only if the renderer's client decodes it (not needed for the PoC).
  A renderer advertises what its client decodes, e.g. `snapcast:*:audio/flac:*` and
  `snapcast:*:audio/L16:*`. A matching control point compares the MIME type; the
  parameters tell the renderer what to expect.
- **additionalInfo:** `DLNA.ORG_OP=00` (no byte or time ranges over HTTP; seeking goes
  through the session, below). The DLNA flags mark a live stream where needed; which
  ones control points expect is an item for the PoC.

One item gets **one** Snapcast resource. Two (say `pcm` and `flac`) would be two
sessions fed separately, and renderers picking different ones would not play in sync.
Default codec `flac` (lossless, about half of PCM's bit rate, safer over Wi-Fi). The
sample rate is the item's own (44.1 kHz for a CD rip), so dms does not resample;
Snapcast clients resample to their device anyway.

### Preferring the Snapcast resource (decision 8)

DLNA has no explicit preference. The control point picks one `<res>` that matches the
renderer's `SinkProtocolInfo`, and the HTTP resource matches too. So the preference has
to be built from what control points look at, on three levels:

1. **Order in the listing:** dms lists the `snapcast` resource **first**. Most control
   points take the first resource that matches.
2. **Order in the renderer's `SinkProtocolInfo`:** gmrender lists its `snapcast:` entries
   **first**. Some control points rank resources by the renderer's order.
3. **The renderer decides itself (fallback):** `SetAVTransportURI` also carries
   `CurrentURIMetaData`, the item's DIDL-Lite. If a control point chose the HTTP
   resource but the metadata also lists a `snapcast` resource, gmrender plays the
   Snapcast one instead. This works only with control points that send the item's
   resources in the metadata (many send just the chosen one). It also needs to be a
   configurable switch, so that a single room can still play on its own.

PoC step 1 checks which of these the control points at hand honour.

### URI

```text
snapcast://<host>[:<port>]/<stream>[?control=<port>][&volume=<0..100>]
```

| Part | Default | Meaning |
|---|---|---|
| `host` | (required) | The Snapcast server (the media server's machine), name or IP |
| `port` | `1704` | Its stream port |
| `stream` | (required) | The Snapcast stream (= the session), percent-encoded |
| `control` | `1705` | Its JSON-RPC port. The renderer needs it to join the stream and to send Play/Pause/Seek |
| `volume` | unset | Initial volume of a renderer that joins, in percent |

Format details are not in the URI: they are in `protocolInfo`, and the Snapcast stream
announces them again to every client (its codec header).

On `volume`: the URI is the same for every renderer that joins, so its volume is the
same for all of them. It applies once, when a renderer joins. After that the renderer's
own volume (RenderingControl, below) wins. The Snapcast server remembers each client's
volume across sessions, so without `volume` a room keeps its last level. The PoC should
show whether an initial volume in the URI is worth having (Open questions).

## Sessions

**A session is one Snapcast stream.** All renderers playing it play the same audio at
the same moment; that is what the Snapcast server does.

**Stream name per item** (`dms-<hash of the item id>`): every renderer given the same
item joins the same session. Playing the same item in two rooms independently is
therefore not possible. If that turns out to matter, a `session` part can be added later.

### How a session runs

```text
control point          renderer A / B (gmrender + snapsrc)     daemon           dms
     │ Browse ─────────────────────────────────────────────────────────────────▶ │
     │ GetProtocolInfo ──▶ A: "snapcast:*:audio/flac:*, http-get:…"              │
     │ SetAVTransportURI(snapcast://nas:1704/dms-3f9a1c) ──▶ A                    │
     │                         A: connect 1704, Group.SetStream(dms-3f9a1c) ──▶   │
     │                                                  unknown, prefix "dms-":  │
     │                                                  Open(dms-3f9a1c) ──────▶ │
     │                                                  ◀────── Acquire (paused) │
     │                         A: STOPPED (session ready)                         │
     │ Play ─────────────────▶ A: Stream.Control play ─▶ Command ──────────────▶ │
     │                                                  ◀───── PCM from position │
     │                         A: PLAYING (follows the session's playback state)  │
     │ SetAVTransportURI + Play ──▶ B: same stream, already playing: B joins      │
     │ Pause ────────────────▶ B: Stream.Control pause ─▶ ─────────────────────▶ │
     │                         A and B: PAUSED (Stream.OnProperties)              │
```

- **Start (decision 3):** the control point's `Play` on a renderer becomes
  `Stream.Control play` on the session. dms starts feeding.
- **Pause and seek (decision 4)** on any renderer become `Stream.Control pause` and
  `Stream.Control setPosition`, so they act on the whole session. Every renderer in the
  session follows the session's playback state (`Stream.OnProperties`) and reports it
  through its own AVTransport events. A control point that only knows renderer A
  therefore also sees a pause that came from B.
- **Join (decision 5):** `Play` on a session that is already playing changes nothing.
  The renderer simply joins in.
- **Stop is local** (proposal): `Stop` makes that renderer leave the session; the others
  go on playing. Pause is for the whole session; Stop is how one room drops out. When the
  last renderer has left and the session is paused or stopped, dms closes it after a
  while (idle timeout).
- **End of the track:** dms reports playback `stopped`, every renderer goes to STOPPED,
  and the control point moves on as usual, by `SetAVTransportURI` with the next item
  (or `SetNextAVTransportURI`). Each renderer is told separately but gets the same next
  item, so they meet again in the next session. A renderer that arrives a few hundred
  ms later misses only the first moment, then plays in sync.

### Latency of control

Snapcast clients hold up to `buffer-ms` of audio (default 1000 ms) that the server
already sent. The protocol has no "discard what you have" message. So pause and seek
take effect only after up to `buffer-ms`: what is already buffered still plays. That's
acceptable for music. A smaller `buffer-ms` for these sessions shortens it, and a flush
would be a Snapcast protocol extension (later, if at all).

### Mapping of the renderer's actions

| UPnP action | On the renderer | On the session |
|---|---|---|
| `SetAVTransportURI(snapcast://…)` | `snapsrc` connects, joins the stream | Opened on demand (dms) |
| `Play` | follows the session | `Stream.Control play` (no-op when playing) |
| `Pause` | follows the session | `Stream.Control pause` |
| `Seek(REL_TIME)` | follows the session | `Stream.Control setPosition` |
| `Stop` | leaves the session | none (closed when idle) |
| `GetPositionInfo` | position and duration of the session | from the stream's properties |
| `GetTransportInfo` | the session's playback state | from the stream's properties |
| `Next`/`Previous` | as today (the control point's playlist) | none |
| RenderingControl `SetVolume`/`SetMute` | this room only | `Client.SetVolume` of this renderer's client |

## Media server side (dms)

### Feeding the session, PoC: GStreamer in dms

For the PoC, dms uses GStreamer for Snapcast sessions only (the user, 2026-11-24: not a
clean design, but quick). ffmpeg stays for everything else. dms builds one pipeline per
session in-process, through the Go bindings (`go-gst`):

```text
filesrc location=<item> ! decodebin ! audioconvert ! audioresample ! snapsink stream=dms-3f9a1c mode=auto
```

This reuses everything `snapsink` already does: the daemon, pacing, timestamps, and tags
as stream metadata. It also maps the session's control onto GStreamer itself:

| Session | In dms's pipeline |
|---|---|
| `Stream.Control` from a renderer | `snapsink` posts `snapcast-stream-control` on the bus; dms handles it |
| play, pause | pipeline PLAYING or PAUSED (paused, `snapsink` sends nothing) |
| seek (`setPosition`) | a flushing seek on the pipeline |
| position, duration | position and duration queries; dms hands them to `snapsink` (new: a way to set `position` in the stream's properties, see below) |
| end of track | EOS on the bus: playback `stopped` |

It must be in-process (`go-gst`), not a `gst-launch-1.0` child process. Otherwise the
bus messages with the commands never reach dms. A `snapsink` that was fed while paused
and then resumed, or one that seeks, must keep a continuous timeline on the server. That
needs testing (running time after a flushing seek).

On-demand sessions (below) still need dms to hear "open this stream" from the daemon,
which a `snapsink` cannot pass on. So the PoC first uses streams that dms opens itself
(PoC step 4a). For on-demand sessions, dms opens a small control-only connection to the
daemon (`Provide`, `Open`, `Close`, no PCM).

### Feeding the session, later: ffmpeg and the daemon socket

The clean design without GStreamer in dms: dms already transcodes with ffmpeg. For a
Snapcast session it runs ffmpeg to raw PCM at
the item's rate and feeds it into the daemon over its Unix socket
(`docs/shared-server.md`, the IPC protocol): `Hello`, `Acquire`, then `Pcm` frames,
`Properties` for title, artist, album, duration, position and playback state, and
`Command`s back for play, pause and seek. No GStreamer is needed in dms. The protocol is
small (a frame is type, length and payload; control messages are JSON). A Go client is
a few hundred lines, and is the first producer of the daemon that is not `snapsink`.

- **Timestamps:** each `Pcm` frame carries its capture time on `CLOCK_MONOTONIC`
  (`golang.org/x/sys/unix.ClockGettime`). dms paces the feed in real time, as
  `snapsink` does.
- **Seek:** restart ffmpeg at the new position (`-ss`), report the new position.
- **Pause:** stop feeding, report `paused`. The session stays open.
- **Daemon:** dms runs on the same machine and as the same user as the daemon (the
  socket is private to the user), and starts it if needed, as `snapsink` does
  (`GST_SNAPCAST_DAEMON` or `PATH`). As a system service, both run under dms's service
  user.

### Sessions on demand

A session must exist before a renderer can join it, but dms cannot create one for every
item it lists (a folder can hold hundreds). So the daemon learns to create streams on
demand:

- dms connects once and **registers as the provider** of a stream-name prefix (`dms-`).
- When a client asks to join an unknown stream with that prefix (`Group.SetStream`),
  the daemon asks the provider to **open** it. dms looks the item up by the hash,
  acquires the stream (paused, with its properties), and the daemon then completes the
  join.
- When a session has no clients and is not playing for a while, the daemon tells the
  provider to **close** it.

That is new in two places:

- **The daemon's IPC protocol** (version 3): `Provide { prefix }`, and `Open { stream }`
  and `Close { stream }` from the daemon to the provider.
- **snapcast-rs** (the fork, as an upstream PR candidate): a hook in the server that
  resolves an unknown stream name before `Group.SetStream` fails ("streams on demand").
  Today a stream must exist before a group can be set to it.

`Properties` also needs a `position` (seconds), which it lacks today. Snapcast's stream
properties already define one.

In the PoC (GStreamer in dms), the `Open` reaches dms over that control-only
connection, and dms answers by starting a `snapsink` pipeline for the stream.

## Renderer side (gmrender + snapsrc)

gmrender plays every URI through `playbin`, and passes it on without filtering by scheme
(`output_gstreamer.c`, `output_gstreamer_set_uri`). So most of the work is in `snapsrc`:

- **`snapcast://` URI handler** (GStreamer's `GstURIHandler`): `playbin` then picks
  `snapsrc` for such a URI by itself. The URI sets host, port, stream, control port and
  volume.
- **Joining a stream:** `snapsrc` connects as today, then, via JSON-RPC on the control
  port, finds its own group (`Server.GetStatus`, by its client id) and sets it to the
  stream (`Group.SetStream`). Today `snapsrc` plays whatever the server assigned.
  The engine's `client` feature needs a small JSON-RPC client for this.
- **Session state to the renderer:** `snapsrc` follows the stream's properties
  (`Stream.OnProperties`) and posts them on the bus (playback state, position, duration,
  tags). Playback commands from the application go the other way as `Stream.Control`.
  How gmrender asks for them is a gmrender question (below).
- **While paused** the session sends no audio, and `snapsrc` outputs silence marked GAP,
  as it already does when the server sends nothing (F5).

gmrender needs:

- **`snapcast` in its protocolInfo, first.** It builds its list from the MIME types of
  all installed GStreamer elements' sink pads (`scan_mime_list`), always with the
  `http-get` protocol. Entries like `snapcast:*:audio/flac:*` must be added when
  `snapsrc` is installed (it can ask the registry for a `snapcast` URI handler), and put
  **at the front** of the list (decision 8).
- **Preferring a Snapcast resource from the metadata** (the fallback in "Preferring the
  Snapcast resource"): in `SetAVTransportURI`, look for a `snapcast` `<res>` in
  `CurrentURIMetaData` and play it instead of the HTTP URI, behind an option.
- **Pause, Seek and position for snapcast URIs** go to the session instead of to the
  local `playbin`: on Pause, send `pause` to the session instead of pausing the pipeline;
  report the session's position instead of the pipeline's. The cleanest boundary is
  probably element messages or signals of `snapsrc`, so that gmrender only needs a small
  branch for "the source is a session".
- **Volume:** RenderingControl maps to this renderer's Snapcast client volume, not
  `playbin`'s volume, so that Snapweb and the renderer show the same value.

Licenses: gmrender is GPL-2.0-or-later (the source headers). Loading the GPL-3.0-only
`snapsrc` into it is compatible; the combination is GPL-3.0. dms is BSD-3-Clause and
does not link anything of ours (it talks to the daemon over a socket).

## Video with the audio split off (later)

The media server offers the video (HTTP) and its audio track as a Snapcast session. The
control point sends the video to the TV and the Snapcast resource to the speakers. The
TV plays on its own clock, not on the Snapcast timeline. Lip sync therefore needs:

- the same start: the control point starts both, and the TV's start lag must be measured
  per TV;
- a per-TV offset, much like `snapsrc`'s `latency-ms`, set by hand;
- for pause and seek, both sides must be told. That's the control point's job, so a
  standard control point will not do it. This needs a control point that knows about it.

Worth designing for now (an item may carry a video resource plus a Snapcast audio
resource); not part of the PoC.

## What changes where

| Where | Change | Upstream |
|---|---|---|
| this repo, engine/daemon | IPC v3: `Provide`, `Open`, `Close`; `position` in `Properties` | ours |
| this repo, `snapsrc` | `snapcast://` URI handler; join a stream via JSON-RPC; session state on the bus | ours |
| snapcast-rs (fork) | Streams on demand (a resolver hook before `Group.SetStream` fails) | PR candidate |
| this repo, `snapsink` | A way for the application to set `position` (and playback state) in the stream's properties | ours |
| dms (fork) | The `snapcast` resource; PoC: a `go-gst` pipeline with `snapsink`, play, pause and seek; later maybe the ffmpeg feed via the daemon socket | PR candidate, if dms wants it |
| gmrender-resurrect (fork) | `snapcast` first in protocolInfo; prefer a Snapcast resource from the metadata; session-aware Pause/Seek/position/volume | PR candidate |

## PoC steps

1. **Control point check, no code:** does BubbleUPnP (and one more control point) pass a
   `snapcast` resource to a renderer that advertises it, and does it **prefer** it over
   the HTTP resource (order in the listing, order in `SinkProtocolInfo`)? Does it send
   all of the item's resources in `CurrentURIMetaData` (the renderer-side fallback)? Test with a hand-written DIDL
   item (a static file served by dms or a tiny test server) and a renderer that only
   logs `SetAVTransportURI`. If control points filter the resource out, the design
   needs a fallback (e.g. an `http-get` resource whose URL redirects or describes the
   session). This is the biggest risk, so it comes first.
2. **`snapsrc` URI handler and stream join**, tested with
   `gst-launch-1.0 playbin uri=snapcast://…` against a `snapsink` stream.
3. **gmrender** plays `snapcast://` URIs, advertises them, maps Pause/Seek/position.
   A fixed stream fed by `make listen-send` stands in for dms.
4. **dms** with GStreamer (`go-gst`, `snapsink`):
   a. the resource and a session that dms opens itself for one item (say, the first
      item that is browsed or a configured one), with play, pause and seek on the
      pipeline;
   b. sessions on demand: daemon IPC v3, the fork hook, and dms's control-only
      connection.
   The ffmpeg and daemon-socket feed replaces GStreamer in dms only if the PoC succeeds
   and dms upstream is interested.
5. **Two renderers, one control point**, then the overnight test as for the daemon.

## Repository layout during the PoC

Both projects as **forks** (`aanno/dms`, `aanno/gmrender-resurrect`) and git submodules
under `dlna/` in this repo, like `snapcast-rs`: changes committed in the fork, the
pointer bumped here in its own commit. They are not part of `make check`; they get
their own make targets. The devcontainer needs Go (dms, with cgo for `go-gst`) and
libupnp plus autotools (gmrender). After the PoC, the DLNA integration may move into a repo of its own.

## Open questions

1. Do control points pass a `snapcast` resource through, and prefer it (PoC step 1)?
   Which DLNA flags do they expect for a live stream?
2. Is the initial `volume` in the URI worth having, given that it's the same for every
   renderer and the Snapcast server remembers each room's volume?
3. One session per item: is independent playback of the same item in two rooms needed?
4. Stop leaves the session (proposal above): or should Stop stop the session for all?
5. Does dms resample to one rate per session, or keep the item's own rate (proposal)?
   Gapless playback across tracks with different rates is out of scope either way.
