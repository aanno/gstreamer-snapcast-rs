# Design: a shared Snapcast server across processes

Status: **draft for discussion** (2026-11-22). Nothing here is implemented yet. The
outline was agreed with the project owner on 2026-11-22 (`docs/claude/memory.md`,
Decisions); this document fills in the details and lists what is still to decide.

## Goal

Today every process with a `snapsink` runs its own embedded Snapcast server. Several
`snapsink`s in **one** process share a server per port, each with its own stream (L1,
done). Across processes they cannot: a second process on the same port fails to bind.

Wanted: the first pipeline with a `snapsink` starts one server; every further pipeline,
in any process of the same user, adds its stream to it; the server ends with the last
pipeline. The control API and Snapweb, if enabled, belong to that one server.

Agreed constraints:

- **Per user**, **autospawn only** (no systemd unit, no system service).
- **Default mode is shared.** Embedded stays available, and is the default where no
  daemon can be spawned (Android).
- **The timing work survives:** audio crosses the process boundary with its server
  timestamp, never re-timed on arrival.
- **No leader election** inside the pipelines: the server must not die with whichever
  pipeline happened to start it.

## Overview

```text
  process A (gst-launch, Mopidy, …)          process B
  ┌───────────────────────────────┐          ┌─────────────────────────┐
  │ … ! snapsink stream=music     │          │ … ! snapsink stream=tv  │
  │     engine: shared client     │          │   engine: shared client │
  └──────────────┬────────────────┘          └────────────┬────────────┘
                 │ Unix socket $XDG_RUNTIME_DIR/gst-snapcast/daemon.sock
                 ▼                                        ▼
  ┌──────────────────────────────────────────────────────────────────┐
  │ gst-snapcast-daemon (no GStreamer)                               │
  │   one IPC connection = one acquire_stream() of the engine        │
  │   engine registry: SnapServer per port, streams added at runtime │
  │   control API, Snapweb, mDNS                                     │
  └──────────────────────────────┬───────────────────────────────────┘
                                 │ Snapcast protocol (TCP 1704), JSON-RPC (1705/1780)
                                 ▼
                    snapsrc, C++ snapclient, Snapweb, …
```

The daemon is a thin process around the **existing engine**: each IPC connection does
what an embedded `snapsink` does today, `server::acquire_stream` with the sink's
options, and the stream is released when the connection closes. Everything that works
embedded (several streams per port, codecs, control API, Snapweb, mDNS, metadata,
`Stream.Control`) works in the daemon without new server code.

## Components

| Part | Where | GStreamer? |
|---|---|---|
| IPC protocol (frames, messages) | `gst-snapcast-engine`, new module `ipc`, feature `shared` | no |
| Client side: connect or spawn, a `StreamHandle` backed by a socket | `gst-snapcast-engine`, feature `shared` | no |
| Daemon binary | new crate `crates/gst-snapcast-daemon` | no |
| `mode` property | `snapsink` | yes (translation only) |

The engine keeps one public entry point. `acquire_stream` gets a mode and returns the
same `StreamHandle` API as today (`send_pcm`, `set_properties`, `on_command`, ports), with
an embedded or a shared backend behind it. `snapsink` stays a translator, as the
architecture rules require. The `shared` feature is Unix-only (`std::os::unix`) and off
in the Android build.

## Discovery, autospawn and the singleton

- **Socket:** `$XDG_RUNTIME_DIR/gst-snapcast/daemon.sock`. `XDG_RUNTIME_DIR` is per user
  and mode 0700, so the socket is private without further checks. Without
  `XDG_RUNTIME_DIR`: shared mode is unavailable (see `mode`).
- **One daemon per user, for all ports.** The engine registry inside it already keys
  servers by port.
- **Singleton:** the daemon takes an exclusive `flock` on
  `$XDG_RUNTIME_DIR/gst-snapcast/daemon.lock` for its whole life before it binds the
  socket. A second daemon fails to get the lock and exits at once. A stale socket file
  (left by a crashed daemon) is removed by the daemon that holds the lock.
- **Connect or spawn** (in the engine, at `acquire_stream`):
  1. connect; on success, done;
  2. otherwise spawn the daemon and retry connecting for up to 2 s (50 ms steps);
  3. if several sinks spawn at once, the lock lets one daemon win; the others' spawns
     exit, and all sinks connect to the winner.
- **Spawning:** the daemon is started detached (`setsid`, stdin/stdout to `/dev/null`).
  Without that, Ctrl+C on `gst-launch-1.0` would send SIGINT to the daemon too (same
  process group) and kill every other pipeline's stream.
- **Finding the binary:** `GST_SNAPCAST_DAEMON` (path), else `gst-snapcast-daemon` on
  `PATH`. `make install` puts it in `~/.local/bin` (per user) or `$(bindir)` (system).

## Lifetime

- The daemon counts IPC connections. When the last one closes, it waits a **grace
  period** (default 5 s, `--grace-secs`) and exits if nobody connected meanwhile. The
  grace period covers sinks that restart (READY → PAUSED again, an application that
  rebuilds its pipeline between tracks), so clients don't lose the server, its stream
  assignment or the control connection.
- **A pipeline crashes:** the kernel closes its socket; the daemon releases the stream
  like a normal stop. No heartbeat needed.
- **The daemon crashes:** every sink's next write fails. The sink reconnects once
  (which respawns the daemon) and re-acquires its stream; if that fails, it posts an
  element error. Snapcast clients reconnect by themselves (snapsrc and the C++ client
  both do), but their group assignments are lost with the daemon's state (no
  persistence, as today).
- **Released streams** stay on the server, unclaimed, until the server stops (the L1
  behaviour), so a restarting sender keeps its clients. With a long-lived daemon this
  means idle streams can linger in Snapweb while other sinks keep the daemon alive.
  Acceptable for now; a later option could remove a stream some time after release.

## IPC protocol

A Unix stream socket. Each message is a frame: `type: u8`, `length: u32` (little
endian), payload. Control messages carry JSON (the engine already depends on
`serde_json`); PCM carries raw bytes. No new dependencies.

| Direction | Message | Payload |
|---|---|---|
| sink → daemon | `Hello` | protocol version, pid, `log_id` |
| daemon → sink | `Welcome` | daemon version |
| sink → daemon | `Acquire` | `ServerOptions`, `StreamOptions` (as today) |
| daemon → sink | `Acquired` | bound port, control ports, the effective server options |
| daemon → sink | `Error` | the `EngineError` as text (and a kind, so the sink can map it) |
| sink → daemon | `Pcm` | `timestamp_usec: i64`, interleaved PCM |
| sink → daemon | `Properties` | `StreamProperties` |
| daemon → sink | `Command` | `StreamCommand` (`Stream.Control` from a controller) |
| either | close | release |

- **Versioning:** `Hello` carries a protocol version; a mismatch is an error naming both
  versions. This happens when an old daemon from a previous install still runs; it
  exits by itself after its last client, so retrying later works.
- **Server-wide options** (`buffer-ms`, control ports, `snapweb-dir`, `mdns-name`) are
  set by the first acquirer of a port, as today. Across processes that is less
  obvious, so `Acquired` returns the effective options and the sink logs a warning when
  its own differ.
- **Errors keep their meaning:** a taken stream name, a taken port, a rejected codec
  reach the sink as the same `EngineError` kinds as in embedded mode, and become the
  same element errors.

## Timing

The engine's time base is `CLOCK_MONOTONIC` in µs (`now_usec`), and that clock is
system-wide: a timestamp taken in the sink's process means the same instant in the
daemon. So:

- `snapsink` computes the server timestamp exactly as today
  (`now_usec() + render_clock_time − clock.time()`), and the daemon passes it on
  unchanged. Arrival time at the daemon plays no role, so the extra hop adds no error.
- **Pacing** (`MAX_AHEAD_USEC`, 200 ms) stays in `snapsink`; the daemon adds no buffer
  of its own beyond its stream channel.
- **Backpressure:** the daemon reads a connection only as fast as its stream channel
  accepts frames, so a full channel fills the socket buffer and blocks the sink's
  write, as `blocking_send` blocks today. The write gets a bound (a send timeout, 2 s)
  and is interrupted by `unlock()` (flushing), as the rules require.
- **Cost:** a stream of 48 kHz, 16 bit, stereo is 192 kB/s, copied once through a
  socket. That is negligible; shared memory or lock-free rings would add complexity
  without a measurable gain here.
- **Check:** an aloop run with the sender in shared mode must give the same offsets as
  embedded, within the usual noise.
- **Caveat:** processes in different time namespaces (Linux `CLONE_NEWTIME`) see
  different `CLOCK_MONOTONIC` values. Desktop sandboxes don't use them today; if one
  does, shared mode is wrong for it and embedded mode is the fallback.

## The `mode` property of `snapsink`

| Value | Behaviour |
|---|---|
| `auto` (default) | Shared if the daemon can be reached or spawned; otherwise embedded, with a warning saying why. In builds without the `shared` feature (Android): embedded. |
| `shared` | Shared or an error. |
| `embedded` | Today's behaviour: the server in this process. |

`mode` is `.mutable_ready()`, like the other server options. `port=0` (ephemeral, for
tests) implies embedded, because an ephemeral port is never shared anyway.

An embedded sink and the daemon can collide on a port: whichever binds first wins, and
the other gets the usual "cannot listen on port" error.

## Logging

- The daemon installs its own `tracing` subscriber (it is a binary; the engine still
  never installs one). Output goes to `$XDG_RUNTIME_DIR/gst-snapcast/daemon.log`,
  level from `GST_SNAPCAST_DAEMON_LOG` (an env-filter string, default `info`).
- The sink side keeps logging through `gst-snapcast-common` into the GStreamer debug log
  (`snapcast-engine`), including the IPC client. What the server logs about a stream
  stays in the daemon log; errors that matter to a sink are sent to it (`Error`) and
  posted on its bus.
- `docs/architecture.md` gets the daemon in its thread and interface sections, and its
  environment variables in the table.

## Testing

- **Unit:** frame encoding and decoding; version mismatch; the lock-file singleton.
- **Engine integration** (no GStreamer): two acquisitions over IPC share a port and list
  both streams (JSON-RPC); a closed connection releases its stream; the daemon exits
  after the grace period; concurrent spawns give one daemon; a killed client process
  releases its stream.
- **Plugin integration:** two `gst-launch-1.0` processes with `snapsink mode=shared` on one
  port and a `snapsrc` receiving; Ctrl+C on the first one leaves the second playing.
- **Timing:** an aloop variant with the sender in shared mode (`make test-aloop`), compared
  with embedded.
- Tests use their own `XDG_RUNTIME_DIR` (a temp dir) and the daemon from
  `target/debug`, so they don't meet a user's running daemon and can run in parallel.

## Steps

1. Protocol module and the daemon with `Hello`/`Acquire`/`Pcm`/close, started by hand;
   engine tests.
2. Engine client backend and `acquire_stream` with a mode; `snapsink`'s `mode`.
3. Autospawn, singleton, grace period, reconnect after a daemon crash.
4. `Properties` and `Command` (metadata and `Stream.Control` through the daemon).
5. Plugin and aloop tests; `make install`, `make doctor`, `scripts/listen.sh`, docs.

Each step ends with `make check` green and a commit. No snapcast-rs change is expected.

## Alternatives considered

- **Leader election among the pipelines** (the first one hosts, the others connect):
  the server would die with the first pipeline. Rejected.
- **C++ snapserver with a pipe or TCP source:** loses the timestamps (the source is
  timed by arrival), so the timing work would not survive. Rejected.
- **systemd user service or socket activation:** rejected for now (autospawn only);
  socket activation could be added later without changing the protocol.
- **D-Bus:** no session bus on Android, more dependencies, no gain over a private Unix
  socket. Rejected.
- **Shared memory for PCM:** not needed at these data rates (see Timing).

## Open questions

1. **Names:** crate and binary `gst-snapcast-daemon`, socket directory `gst-snapcast/`?
2. **`mode=auto` falls back to embedded with a warning.** Or should the default be strict
   `shared`, failing when the daemon cannot start?
3. **Grace period** 5 s?
4. **Released streams** stay until the daemon exits (as embedded today), or are removed
   after a while?
5. **Daemon crash:** reconnect once (respawning the daemon) as proposed, or post an error
   at once?
