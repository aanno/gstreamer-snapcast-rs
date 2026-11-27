# DLNA/UPnP with Snapcast streams (design)

Status: **agreed** (2026-11-24); the way the session is offered changed twice after
tests with BubbleUPnP. Since **2026-11-27** it is **HTTP content negotiation on the
item's ordinary resource**, with `snapcast-tcp` as the name of the transport.
**PoC steps 2 and 3 are done** (2026-11-27):

- `snapsrc` plays `snapcast-tcp://` URIs and joins the session's stream.
- dms answers the negotiation.
- gmrender asks before playing, and plays the session.
- Pause, Seek, position, volume and the end of a track go through the session.

Next is step 4: dms feeds a session per item.

The wire protocol, for anyone implementing it in another server or renderer, is in
[dlna-snapcast-protocol.md](dlna-snapcast-protocol.md). Tests are in
[dlna-tests.md](dlna-tests.md).

Targets for the proof of concept:

- the media server [dms](https://github.com/anacrolix/dms) (Go);
- the renderer [gmrender-resurrect](https://github.com/hzeller/gmrender-resurrect)
  (C, GStreamer).

Both are used at home and work there. Background:
`docs/gstreamer-plugins/dlna-rs-gstreamer-1.md` (the original idea of a Snapcast URI
and `snapsrc` in gmrender).

## Goal

Synchronized playback on several DLNA renderers, driven by ordinary DLNA control points,
with as little new protocol as possible:

- The **media server** offers a Snapcast session for an item, next to the media
  itself.
- **Negotiation** decides what a renderer gets. A renderer without Snapcast support
  gets the media, as it does today.
- Renderers that get the same item play **one shared session**, in sync.
- **Control points stay as they are.**

## Decisions

From 2026-11-24 (the user), with later changes marked:

1. **The media server provides the stream**, not a renderer: it is the media server's
   job to give the renderer a URL for the media, and to serve the media itself when
   needed (transcoding). A Snapcast session is one more such offer. (A gmrender instance
   with `snapsink` as its output, as a "virtual renderer", was discussed and rejected:
   it works around DLNA instead of using it.)
2. **Negotiation decides, and the media is the fallback.** *Changed 2026-11-27:* it
   is HTTP content negotiation on the resource URL, not `protocolInfo`, because
   control points pass on only what they know (see "How we got here"). Format details
   aren't negotiated: the Snapcast stream announces its codec and format to each client.
3. **The control point starts and pauses the stream**, as in any DLNA use case.
4. **Pause and seek are global:** they act on the session, for all renderers in it.
5. **A second renderer joins** a running session.
6. **The URI** names host, port and stream (and the control port). An initial volume
   was considered and dropped: it would be the same for every room, and the Snapcast
   server already remembers each room's volume.
7. **dms uses GStreamer for the PoC** (`snapsink` in a `go-gst` pipeline) for
   Snapcast sessions; ffmpeg stays for everything else. Not a clean design, but quick.
8. **The renderer must know the new type and prefer it.** *Since 2026-11-27:* a
   renderer with sessions enabled (gmrender `--snapcast-sessions`) asks for a
   session first and plays it whenever the server offers one.
9. **Video with the audio split off** (video to a TV, audio as Snapcast to several
   devices) is a valid scenario. It is designed for here but comes after audio.
10. **Naming** (2026-11-27): not plain "snapcast" but the transport, `snapcast-tcp`:
    the URI scheme `snapcast-tcp://`, the MIME type `audio/x-snapcast-tcp`.
    `snapcast-ws` is reserved for Snapcast over WebSocket.
11. **Transport variants** (2026-11-27): the renderer's `Accept` header lists the
    transports it can play, and the server picks one. The renderer's list comes from
    the GStreamer registry (URI handlers). The server's list comes from its Snapcast
    server, by hand in the PoC, later published by `snapsink`.
12. **One session per item, Stop leaves, the item's own sample rate** (2026-11-24):
    see "Sessions".

## Roles

| Role | DLNA | Here | Changes |
|---|---|---|---|
| Media server | ContentDirectory, HTTP | dms | Answers the negotiation; feeds the session into the Snapcast server |
| Snapcast server | none | `gst-snapcast-daemon` on the media server's machine | Serves the sessions (started by `snapsink`) |
| Renderer | AVTransport, RenderingControl, ConnectionManager | gmrender + `snapsrc` | Asks for a session; plays `snapcast-tcp://` URIs; maps actions to the session |
| Control point | Browse, picks the resource, Play/Pause/Seek | BubbleUPnP, any | **None** |

## Offering the session

Details: [dlna-snapcast-protocol.md](dlna-snapcast-protocol.md). In short:

1. The listing is unchanged: the item has its ordinary `http-get` resource.
2. The control point sends that URL to the renderer, as always.
3. Before playing it, the renderer sends a `HEAD` request for it with
   `Accept: audio/x-snapcast-tcp` (one MIME type for each transport it can play).
4. A media server that offers a session answers with that `Content-Type`. The
   renderer then sends a `GET` with the same header and gets the session URI
   (one line):

   ```text
   snapcast-tcp://<host>[:<port>]/<stream>[?control=<port>]
   ```

   | Part | Default | Meaning |
   |---|---|---|
   | `host` | (required) | The Snapcast server (the media server's machine), name or IP |
   | `port` | `1704` | Its stream port |
   | `stream` | (required) | The Snapcast stream (= the session), percent-encoded |
   | `control` | port + 1 | Its JSON-RPC port, to join the stream (and later to send Play/Pause/Seek) |

5. The renderer plays that URI. `snapsrc` connects and joins the stream.
6. Any other server answers the `HEAD` with the media's type, and the renderer plays
   the URL as usual.

The renderer's `GET` is also dms's cue to **open the session**. That makes sessions
on demand simple (below).

### How we got here

| Date | Design | Result with BubbleUPnP (DLNA test) |
|---|---|---|
| 2026-11-24 | A second `<res>` with its own protocol, `snapcast:*:audio/flac:*`, listed first by dms and by the renderer | Never sent to the renderer, no fallback (tests 1, 7) |
| 2026-11-25 | A second `<res>` as `http-get` with its own MIME type `audio/x-snapcast`, whose URL answers with the session URI | Never sent either (test 8, confirmed 2026-11-27) |
| 2026-11-27 | The same with a type BubbleUPnP knows (`audio/flac`, `audio/x-scpls`) | Reaches gmrender (tests 9a, 9b). But any renderer would get the one-line answer instead of music, and nothing in the DIDL can mark it as a session |
| 2026-11-27 | **Content negotiation on the ordinary resource** | Nothing changes for the control point; built and tested by hand (PoC step 2) |

Other findings from those tests:

- BubbleUPnP sends its own DIDL in `CurrentURIMetaData`, with only the chosen
  `<res>`. It rewrites the fourth protocolInfo field.
- Before sending a URL to the renderer, BubbleUPnP probes it once.
- It may reach dms over IPv6, so dms answers with an address the Snapcast server
  listens on (IPv4 if the interface has one).
- No other control point worked here (test 4), so the design must work with
  BubbleUPnP.

## Sessions

**A session is one Snapcast stream.** All renderers playing it play the same audio at
the same moment; that is what the Snapcast server does.

**Stream name per item** (`dms-<hash of the item id>`): every renderer given the same
item joins the same session. Playing the same item in two rooms independently is
therefore not possible, and not needed (2026-11-24). The sample rate is the item's own
(dms does not resample; Snapcast clients resample to their device anyway). The default
codec is `flac`: lossless, about half of PCM's bit rate, and safer over Wi-Fi.

### How a session runs

Built on the renderer's side (steps 2 and 3). dms opening and feeding a session per
item is step 4; until then, `make dlna-session` stands in for it.

```text
control point        renderer A / B (gmrender + snapsrc)          dms (+ snapsink, daemon)
   │ Browse ───────────────────────────────────────────────────────────────────▶ │
   │ SetAVTransportURI(http://nas:1338/res?path=song.flac) ──▶ A                  │
   │                      A: HEAD/GET, Accept: audio/x-snapcast-tcp ───────────▶ │
   │                                         opens session dms-3f9a1c (paused) │
   │                      A ◀── snapcast-tcp://nas:1704/dms-3f9a1c?control=1705  │
   │                      A: connect 1704, Group.SetStream(dms-3f9a1c)            │
   │                      A: STOPPED (session ready)                              │
   │ Play ──────────────▶ A: Stream.Control play ─────────────────────────────▶ │
   │                                                  ◀────── audio from position │
   │                      A: PLAYING (follows the session's playback state)       │
   │ SetAVTransportURI + Play ──▶ B: same item → same session, already playing    │
   │ Pause ─────────────▶ B: Stream.Control pause ────────────────────────────▶ │
   │                      A and B: PAUSED (Stream.OnProperties)                   │
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
- **Stop is local** (decided 2026-11-24): `Stop` makes that renderer leave the
  session; the others go on playing. Pause is for the whole session; Stop is how one
  room drops out. When the last renderer has left and the session is paused or
  stopped, dms closes it after a while (idle timeout).
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
| `SetAVTransportURI(http://…)` | Negotiation; with a session: `snapsrc` connects and joins the stream | Opened by dms on the `GET` (step 4) |
| `Play` | starts or resumes the session; playbin plays | `Stream.Control play` (no-op when playing) |
| `Pause` | playbin keeps playing (silence) | `Stream.Control pause` |
| `Seek(REL_TIME)` | nothing locally | `Stream.Control setPosition` |
| `Stop` | leaves the session (playbin to READY) | none (closed when idle) |
| `GetPositionInfo` | the session's position (extrapolated between updates) and duration | from the stream's properties |
| `GetTransportInfo` | follows the session: paused or resumed elsewhere moves the state | from the stream's properties |
| end of the track | `stopped` after `playing` counts as EOS: the next URI, or STOPPED | the sender reports `stopped` |
| `Next`/`Previous` | as today (the control point's playlist) | none |
| RenderingControl `SetVolume`/`SetMute` | this room only; playbin stays at full volume | `Client.SetVolume` of this renderer's client, with the control point's 0..100 level as percent |

All of this is built (steps 2 and 3, `docs/dlna-tests.md` test 11).

The renderer keeps reporting the control point's URL as `AVTransportURI`, never the
session URI.

## Media server side (dms)

### The negotiation (built)

`dlna/dms/snapcast.go` in the fork:

- `-snapcastStream <uri>` gives the session URI for a transport. It can be repeated,
  once per transport, and `{host}` stands for this machine.
- A request for an audio item's `/res` whose `Accept` header names one of those
  transports gets the best match instead of the media.
- In the PoC, the URI is fixed (one stream for every item), and something else feeds
  that stream (e.g. `make listen-send`). Stream names per item and feeding come with
  step 4.

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

### Feeding the session, later: ffmpeg and the daemon socket

The clean design without GStreamer in dms: dms already transcodes with ffmpeg. For a
Snapcast session it runs ffmpeg to raw PCM at the item's rate and feeds it into the
daemon over its Unix socket (`docs/shared-server.md`, the IPC protocol): `Hello`,
`Acquire`, then `Pcm` frames, `Properties` for title, artist, album, duration, position
and playback state, and `Command`s back for play, pause and seek. No GStreamer is
needed in dms. The protocol is small (a frame is type, length and payload; control
messages are JSON). A Go client is a few hundred lines, and is the first producer of
the daemon that is not `snapsink`.

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
item it lists (a folder can hold hundreds). With content negotiation, **the renderer's
`GET` tells dms which item to open**: dms starts the session (a `snapsink` pipeline,
paused, with the item's tags) and answers once the stream exists. `snapsrc` retries the
join for a few seconds anyway. When a session has had no clients for a while, dms
closes it.

This replaces the earlier plan (2026-11-24), in which the daemon asked dms to open
unknown streams: a daemon IPC v3 (`Provide`/`Open`/`Close`) and a "streams on demand"
hook in snapcast-rs. Neither is needed now.

What is still needed: `Properties` gets a `position` (seconds), which it lacks
today. Snapcast's stream properties already define one.

### The feeder behind an interface (proposal, 2026-11-27)

The user asked whether dms's media backend could be made configurable (GStreamer or
ffmpeg, by a contract). My proposal: yes, but only for the **session feed**, not
for all of dms's media handling.

- Probing, transcoding and thumbnails are ffmpeg/ffprobe throughout dms. Its
  transcode specs are ffmpeg arguments, and ffprobe's output feeds the listing.
  Abstracting all of that is a large refactor, with little gain and little chance
  upstream takes it.
- A small Go interface is enough, e.g.
  `SessionFeeder { Open(item, stream) (Session, error) }` with
  `Session { Play(); Pause(); Seek(pos); Position(); Close() }`, and two
  implementations:
  - `gst`: go-gst plus `snapsink`, like `session-sender`;
  - `ffmpeg`: PCM into the daemon socket.
- dms picks one with a flag (`-snapcastFeeder gst|ffmpeg`). The gst feeder sits
  behind a Go build tag, so dms still builds without cgo and GStreamer. It looks up
  `snapsink` at startup and warns if it's missing, like gmrender does for `snapsrc`.

To be decided at the start of step 4.

### Which transports (later)

dms must know which transports its Snapcast server serves, and on which ports. In the
PoC it gets them by hand (`-snapcastStream`, once per transport). Later, `snapsink`
(or the daemon) should publish them: for example, a read-only property with one URI
template per transport the server listens on. dms then needs no configuration
beyond turning sessions on.

## Renderer side (gmrender + snapsrc)

### Built (PoC step 2)

- **`snapsrc`:**
  - It is the `GstURIHandler` for `snapcast-tcp://` (`src/snapsrc/uri.rs`), so
    `playbin` picks it by itself.
  - The URI sets the properties `host`, `port`, `stream` and `control-port`.
  - With `stream` set, the engine's client joins that stream via JSON-RPC
    (`Server.GetStatus` to find its group by its client id, then `Group.SetStream`).
    It does this after every (re)connect and retries for a few seconds
    (`gst-snapcast-engine`, `src/client/join.rs`).
  - Test: `control.rs`, `playbin_plays_a_snapcast_tcp_uri_and_joins_its_stream`.
- **gmrender** (`src/snapcast_session.c`, option `--snapcast-sessions`):
  - Before an `http://` URI goes to the output (`output_set_uri`,
    `output_set_next_uri`), gmrender runs the negotiation and hands the
    `snapcast-tcp://` URI to `playbin` instead.
  - Its `Accept` header lists the transports whose URI scheme an installed GStreamer
    source handles. Without `snapsrc`, it doesn't negotiate at all.

### Built (PoC step 3)

- **`snapsrc` follows the session** (module docs, "Sessions"):
  - The engine keeps the control connection open after the join (`client/session.rs`).
    It follows the stream's properties (`Stream.OnProperties` and friends), and
    `snapsrc` posts them as the element message `snapcast-session`: playback status,
    position, duration, title, what the stream accepts.
  - Commands go the other way through action signals: `stream-control(command,
    params)` (`Stream.Control`) and `set-server-volume(percent, mute)`
    (`Client.SetVolume`).
  - While paused, the session sends no audio, and `snapsrc` outputs silence marked
    GAP (F5).
- **`snapsink` reports what a renderer shows:**
  - the position: whole seconds of stream time, minus its buffer, so it's what
    listeners hear;
  - the duration: from the tags, or else a query upstream;
  - `stopped` after EOS.
- **gmrender** (`output_gstreamer.c`) has a small branch for "the source is a
  session": the mapping table above. A pause or resume elsewhere moves its transport
  state (new output feedback `PLAY_PAUSED`/`PLAY_RESUMED`).
- **The stand-in sender** `make dlna-session` (`snapsink`'s example
  `session-sender`) plays a file into a stream and obeys play, pause, seek and stop,
  as dms's feeder will.

Known limits:

- Gapless `SetNextAVTransportURI` into a session isn't handled (playbin's
  `about-to-finish` path). The next track starts after `stopped` instead.
- Pause and seek take effect after up to the Snapcast buffer ("Latency of control").
- The level sounds a little different in a session than with gmrender's own volume.
  gmrender maps 0..100 to −60…0 dB; `snapsrc` uses the C++ snapclient's curve
  (since 2026-11-27, test 11d), e.g. 50 ≈ −12 dB there vs −20 dB in gmrender.

Licenses: gmrender is GPL-2.0-or-later (the source headers). Loading the GPL-3.0-only
`snapsrc` into it is compatible; the combination is GPL-3.0. dms is BSD-3-Clause and
does not link anything of ours (it talks to the daemon over a socket).

## Video with the audio split off (later)

The media server offers the video (HTTP), and its audio track as a Snapcast session.
The control point sends the video to the TV and the item to the speakers. The speakers
negotiate the session. The TV plays on its own clock, not on the Snapcast timeline.
Lip sync therefore needs:

- the same start: the control point starts both, and the TV's start lag must be measured
  per TV;
- a per-TV offset, much like `snapsrc`'s `latency-ms`, set by hand;
- for pause and seek, both sides must be told. That's the control point's job, so a
  standard control point will not do it. This needs a control point that knows about it.

Worth designing for now; not part of the PoC.

## What changes where

| Where | Change | Status | Upstream |
|---|---|---|---|
| this repo, `snapsrc` + engine | `snapcast-tcp://` URI handler; join a stream via JSON-RPC | **done** | ours |
| this repo, `snapsrc` + engine | Session state on the bus; `Stream.Control` and `Client.SetVolume` by action signals | **done** | ours |
| this repo, `snapsink` | `position`, duration from a query, `stopped` after EOS; the `session-sender` example | **done** | ours |
| this repo, `snapsink` | Publish the session URI templates per transport | later | ours |
| dms (fork) | Negotiation, transport by `Accept` | **done** | PR candidate, if dms wants it |
| dms (fork) | Sessions per item: a `go-gst` pipeline with `snapsink`, play, pause, seek; idle close; later maybe the ffmpeg feed | step 4 | PR candidate, if dms wants it |
| gmrender-resurrect (fork) | Negotiation before playing, `Accept` from the GStreamer registry | **done** | PR candidate |
| gmrender-resurrect (fork) | Session-aware Pause/Seek/position/volume, end of track | **done** | PR candidate |
| snapcast-rs (fork) | WebSocket audio (L11), then `snapcast-ws` in both plugins | later | PR candidate |

## PoC steps

1. ~~**Control point check**~~ done 2026-11-27 (DLNA tests 0–9): BubbleUPnP passes
   only resources it knows, so the design moved to content negotiation (see "How we
   got here").
2. ~~**`snapsrc` URI handler and stream join; the negotiation in dms and
   gmrender**~~ done 2026-11-27. Tested:
   - `playbin` with a `snapcast-tcp://` URI (integration test);
   - by hand, a controller's SOAP requests → gmrender → dms negotiation → `snapsrc`
     on the right stream, with the HTTP fallback.

   - with BubbleUPnP on the host: DLNA test 10, passed the same day.
3. ~~**gmrender** maps Pause/Seek/position/volume to the session~~ done 2026-11-27.
   `make dlna-session` stands in for dms. Tested by hand in the devcontainer (SOAP);
   test 11 is for BubbleUPnP on the host.
4. **dms** feeds a session per item, opened on the renderer's `GET`, with play,
   pause and seek, and an idle close. It does this behind a `SessionFeeder`
   interface (proposal above), with GStreamer (`go-gst`, `snapsink`) first.
   The ffmpeg and daemon-socket feed replaces GStreamer in dms only if the PoC
   succeeds and dms upstream is interested.
5. **Two renderers, one control point**, then the overnight test as for the daemon.

### Running the PoC

Setup, numbered tests and suggestions: [dlna-tests.md](dlna-tests.md).

## Repository layout during the PoC

Both projects as **forks** (`aanno/dms`, `aanno/gmrender-resurrect`) and git submodules
under `dlna/` in this repo, like `snapcast-rs`: changes committed in the fork, the
pointer bumped here in its own commit. They are not part of `make check`; they have
their own make target (`make dlna-build`). The devcontainer needs Go (dms, with cgo for
`go-gst`) and libupnp plus autotools (gmrender). After the PoC, the DLNA integration may
move into a repo of its own.

## Open questions

1. How does `snapsink` (or the daemon) publish the session URIs per transport? A
   read-only property, a bus message, or the daemon's status? Decide in step 4.
2. dms's feeder: a `SessionFeeder` interface with `gst` and `ffmpeg` (proposal in
   "The feeder behind an interface")?

Answered:

- Initial `volume` in the URI: dropped (decision 6, 2026-11-24).
- Independent playback of the same item in two rooms: not needed; one session per item
  (2026-11-24).
- Stop: leaves the session; the others play on (2026-11-24).
- Sample rate: the item's own; dms does not resample (2026-11-24).
- Do control points pass a Snapcast resource through? No (BubbleUPnP), hence content
  negotiation (2026-11-27).
- Does the negotiation work with BubbleUPnP? Yes (DLNA test 10, 2026-11-27).
- Transport variants: through `Accept` and q values; the lists come from GStreamer
  (2026-11-27).
