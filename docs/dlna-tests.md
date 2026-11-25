# DLNA PoC: tests and suggestions

Tests for the DLNA/UPnP proof of concept (design: [dlna.md](dlna.md)). Refer to them by
number ("DLNA test 3"). Numbers stay fixed: new tests are appended, and a finished test
keeps its number and gets a result line (date, what happened).

## Setup

Build both forks into `target/dlna` (needs Go, libupnp, autotools and GStreamer's
development files; all are in the devcontainer):

```sh
make dlna-build
```

On the Fedora host, `make dlna-build` works too (`sudo dnf install golang libupnp-devel
autoconf automake libtool gstreamer1-devel gstreamer1-plugins-base-devel`). It builds
gmrender out of tree and **installs** it into `target/dlna/gmrender`. A build by hand in
`dlna/gmrender-resurrect` must also be installed, into a prefix of its own:

```sh
./autogen.sh && ./configure --prefix=$HOME/.local/opt/gmrender-poc && make && make install
$HOME/.local/opt/gmrender-poc/bin/gmediarender ...
```

Run from the build tree (`./src/gmediarender`), gmrender has no icons: its description
lists them, but fetching them fails ("Could not stat …grender-64x64.png"). A by-hand
build in place also blocks `make dlna-build` (`make distclean` there first). Write logs
outside the submodule (e.g. `| tee /tmp/gmr.log`), so they don't show up as untracked
files in it.

Terminal 1, the media server. Every audio item gets a Snapcast session resource first
(`http-get:*:audio/x-snapcast:*`, URL `/snapcast?path=…`), for one fixed stream on this
machine. Fetching that URL answers with the stream's `snapcast://` URI, and dms logs
`snapcast session requested` (since 2026-11-25; before, the resource used the protocol
`snapcast:`, which BubbleUPnP doesn't pass on: tests 1 and 7):

```sh
target/dlna/dms -path ~/Music -friendlyName "dms snapcast PoC" \
    -snapcastStream 'snapcast://{host}:1704/default?control=1705'
```

Terminal 2, the renderer. `--snapcast-formats audio/x-snapcast` puts
`http-get:*:audio/x-snapcast:*` first in its `SinkProtocolInfo`; `-I` is the network interface for UPnP, `-p` fixes its
HTTP port (easier for the firewall):

```sh
target/dlna/gmediarender -f "gmrender snapcast PoC" -I eno1 --logfile stdout \
    -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c01 -p 49500 \
    --snapcast-formats audio/x-snapcast 2>&1 | grep --line-buffered -E 'AVTransportURI|snapcast|Registered'
```

**Own UUID and port** (`-u`, `-p`): every gmrender build without a compile-time UUID
uses `GMediaRender-1_0-000-000-002` and port 49494. A second gmrender on the LAN (e.g.
the one installed on the host) then looks like the same device, and control points show
only one of them (2026-11-25: the PoC renderer did not appear in BubbleUPnP next to the
installed one).

### Firewall (Fedora host)

Only if firewalld runs (the user's host has none, 2026-11-25). Discovery uses SSDP:
control points send a search to UDP 1900 (multicast) and wait for answers. Fedora's
default zone (FedoraWorkstation) opens 1025–65535 but not 1900.

```sh
sudo firewall-cmd --get-active-zones; sudo firewall-cmd --list-all
sudo firewall-cmd --add-service=ssdp                 # add --permanent to keep it
sudo firewall-cmd --add-port=49500/tcp --add-port=1338/tcp   # gmrender, dms (if not open)
```

## Tests

0. **Discovery.** Start gmrender (and dms) as above on the host. BubbleUPnP lists
   "gmrender snapcast PoC" as a renderer and "dms snapcast PoC" as a library (rescan
   after starting). If not:
   - Another gmrender on the LAN with the same UUID? Then use `-u` (above).
   - gmrender's log has `Registered IPv4 <ip>:49500`? If not, `-I` is wrong.
   - `http://<host-ip>:49500/description.xml` opens on the phone? If not, TCP is blocked.
   - `gssdp-discover -i <iface> --timeout=5` (package `gupnp-tools`) on another Linux
     machine lists it? If not, multicast doesn't get through (firewall above; Wi-Fi
     client isolation or a guest network).
   Record which fix was needed; it goes into the gotchas of `testing-host.md` (done).
   - The PoC gmrender runs installed (icons), not from its build tree (Setup)?
   Result 2026-11-24: not found. 2026-11-25: the host has no firewall; the installed
   gmrender (`-f classic`) appears in BubbleUPnP, the PoC build run from its build tree
   (`./src/gmediarender`) does not. Both used the default UUID; BubbleUPnP fetched the
   classic one's icon (a sign it added the device), the PoC's icons were missing. From
   the devcontainer, the PoC build with its own UUID answers an SSDP search and serves
   a valid description. **Passed 2026-11-25:** the PoC build, installed into a prefix
   (icons) and with its own UUID (`-u`), appears in BubbleUPnP, also with
   `--snapcast-formats`. Both were changed at once, so which one was needed is open;
   always use both (Setup).
1. **Chosen and preferred (case A).** In BubbleUPnP, pick the renderer, browse dms, play
   a track. Nothing sounds yet (no `snapcast://` handler before PoC step 2). Read
   `AVTransportURI:` in gmrender's log: `snapcast://…` means the control point passed
   the resource through and preferred it; `http://…` means it ignored or filtered it.
   Also note any complaint ("unsupported format") or proxying by BubbleUPnP.
   Result 2026-11-25 (BubbleUPnP, dms with `-snapcastStream`, gmrender with
   `--snapcast-formats`): **no `SetAVTransportURI` at all**: BubbleUPnP subscribed to
   the renderer's services but never handed it a track. Narrowing down with test 7.
2. **All resources in the metadata (case B).** In the same log, does
   `AVTransportURIMetaData:` list both `<res>` (snapcast and http), or only the chosen
   one? Both means the renderer-side fallback (gmrender picks the Snapcast resource
   itself) is possible with this control point.
3. **Fallback without Snapcast (case C).** Restart gmrender **without**
   `--snapcast-formats`, play the same track. `AVTransportURI:` must be `http://…` and
   the track must sound: renderers without Snapcast keep working.
4. **A second control point.** Repeat tests 1–3 with another control point (e.g.
   Kodi as a control point, foobar2000/upplay on the desktop, or Hi-Fi Cast). Control
   points differ in how they choose resources; one that works is enough for the PoC,
   but knowing which ones fail tells how much the renderer-side fallback matters.
   Result 2026-11-25: upplay and muspnp tried, neither got working with this setup.
   Another control point is hard to come by; BubbleUPnP is the one in use, so the
   design must work with it (the `http-get` wrapper, test 8).
5. **What does the control point decide by?** Only if test 1 gives `http://`: restart
   dms with `-snapcastFormat 'audio/flac;rate=44100;channels=2'` (and gmrender as
   before), play again. If that changes the result, the control point matches by MIME
   type; if not, it probably drops unknown protocols. Report either way.
6. **dms without a control point.** In `dlna/dms`, `just browse-root` (set `host` in the
   justfile to dms's address) shows the Snapcast `<res>` first for audio items, with the
   host the request used.

7. **Which side stops the control point?** Only if test 1 sends nothing. Play the same
   track in four combinations and look for `AVTransportURI:` in gmrender's log; also
   note what BubbleUPnP shows (error, "unsupported", spinner, plays on the phone):

   | | dms with `-snapcastStream` | dms without |
   |---|---|---|
   | gmrender with `--snapcast-formats` | test 1 | a |
   | gmrender without | b (= test 3) | c (upstream behaviour, must play) |

   a sends `http://`: the item's Snapcast resource is the problem. b sends `http://`:
   the fallback works, and only the combination fails (the control point picks
   `snapcast` but can't handle it). c fails: the setup, not Snapcast.

   Results (BubbleUPnP):
   - **a, 2026-11-25: plays over HTTP.** The renderer's `snapcast:` entries do not
     disturb BubbleUPnP; the problem needs the Snapcast resource in the item.
     Side findings: BubbleUPnP sends **its own DIDL** (own namespaces `sec:`, `pv:`)
     with **only the chosen `<res>`**, so the renderer-side fallback (test 2) is
     unlikely to work with it. It reached dms over **IPv6** (`http://[fd0d:…]:1338`),
     so `{host}` in a `snapcast://` URI would be an IPv6 address too: check that the
     Snapcast server and `snapsrc` handle IPv6 (or let dms prefer IPv4) before step 2.
   - **b, 2026-11-25: plays over HTTP** (`http://192.168.10.139:1338/res?…`): with a
     renderer that doesn't advertise `snapcast`, BubbleUPnP ignores the item's
     Snapcast resource. Renderers without Snapcast keep working.
   - c: not needed (a plays).

   **Conclusion (2026-11-25): BubbleUPnP fails exactly when both sides offer
   `snapcast`:** it matches the resource, apparently chooses it, then sends nothing,
   silently, without falling back to HTTP. It does not pass a `snapcast` protocol
   resource through. Next: test 4 (another control point) and test 8 (the
   `http-get` wrapper).

8. **The `http-get` wrapper** (proposed after test 7, see [dlna.md](dlna.md),
   "Finding: BubbleUPnP and the `snapcast` protocol"). dms offers the session as
   `http-get:*:audio/x-snapcast:*` first, gmrender advertises the same first. Play from
   BubbleUPnP: `AVTransportURI:` must be dms's `/snapcast?…` URL, and the DIDL must
   carry `audio/x-snapcast`. dms's log shows `snapcast session requested` when gmrender
   fetches it. Nothing sounds: gmrender hands the URL to GStreamer, which can't decode
   the one-line answer (an error in the log is expected). Resolving it to the
   `snapcast://` URI and playing that is PoC step 2/3.
   Ready 2026-11-25 (dms `5d1cad6`, gmrender `80b078d`; setup above). Also check the
   old tests in passing: the track must still play over HTTP when gmrender runs
   without `--snapcast-formats` (like test 7b).
   Result 2026-11-25: **no `SetAVTransportURI`** again, and dms never logged
   `snapcast session requested`: BubbleUPnP also refuses an `http-get` resource of a
   MIME type it doesn't know (`audio/x-snapcast`), silently. Next: test 9.

9. **Which MIME type does BubbleUPnP accept for the session resource?** No code
   needed: dms takes the type as `-snapcastFormat`, gmrender advertises what
   `--snapcast-formats` lists. Same check as test 8 (`AVTransportURI:` is dms's
   `/snapcast?path=…` URL):

   | Variant | dms | gmrender |
   |---|---|---|
   | a: a playlist type BubbleUPnP knows | `-snapcastFormat audio/x-scpls` | `--snapcast-formats audio/x-scpls` |
   | b: a plain audio type | `-snapcastFormat audio/flac` | `--snapcast-formats audio/flac` |

   If b works, a cleaner design follows: **content negotiation on one resource** (dms's
   audio URL serves the music to every renderer; a Snapcast-aware renderer asks the
   same URL for the session, e.g. `Accept: audio/x-snapcast`, and gets the
   `snapcast://` URI). No extra resource, no unknown type for the control point.

Later steps get their tests here when they are built: `playbin uri=snapcast://…`
(PoC step 2), gmrender playing it with Pause/Seek going to the session (step 3), dms
feeding a session with `go-gst` (step 4), two renderers in sync (step 5).

## Suggestions

- **Run test 0 first.** Without discovery, the rest says nothing. The firewall is the
  likely cause on the host; the devcontainer shares the host's network and firewall, so
  it behaves the same.
- **Keep the gmrender log** (`--logfile /tmp/gmr.log` instead of the `grep` pipe) for
  tests 1–3, so that the full metadata can be read afterwards and pasted into the
  session.
- **If BubbleUPnP filters the resource:** don't change the design yet. Report what it
  shows; options are the renderer-side fallback (test 2), an `http-get` resource that
  describes the session (dlna.md, PoC step 1), or another control point.
- **Report by number**, e.g. "DLNA test 1: http://, no complaint; test 2: both res".
  The results go into this file and decide the next step in [dlna.md](dlna.md).
