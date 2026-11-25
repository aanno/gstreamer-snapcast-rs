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

Terminal 1, the media server. Every audio item gets a Snapcast resource first,
pointing at one fixed stream on this machine:

```sh
target/dlna/dms -path ~/Music -friendlyName "dms snapcast PoC" \
    -snapcastStream 'snapcast://{host}:1704/default?control=1705'
```

Terminal 2, the renderer. `--snapcast-formats` puts `snapcast:*:audio/L16:*` (and FLAC)
first in its `SinkProtocolInfo`; `-I` is the network interface for UPnP, `-p` fixes its
HTTP port (easier for the firewall):

```sh
target/dlna/gmediarender -f "gmrender snapcast PoC" -I eno1 --logfile stdout \
    -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c01 -p 49500 \
    --snapcast-formats audio/L16,audio/flac 2>&1 | grep --line-buffered -E 'AVTransportURI|snapcast|Registered'
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
   Record which fix was needed; it goes into the gotchas of `testing-host.md`.
   - The PoC gmrender runs installed (icons), not from its build tree (Setup)?
   Result 2026-11-24: not found. 2026-11-25: the host has no firewall; the installed
   gmrender (`-f classic`) appears in BubbleUPnP, the PoC build run from its build tree
   (`./src/gmediarender`) does not. Both used the default UUID; BubbleUPnP fetched the
   classic one's icon (a sign it added the device), the PoC's icons were missing. From
   the devcontainer, the PoC build with its own UUID answers an SSDP search and serves
   a valid description. Next: the PoC build installed (icons) and with `-u`.
1. **Chosen and preferred (case A).** In BubbleUPnP, pick the renderer, browse dms, play
   a track. Nothing sounds yet (no `snapcast://` handler before PoC step 2). Read
   `AVTransportURI:` in gmrender's log: `snapcast://…` means the control point passed
   the resource through and preferred it; `http://…` means it ignored or filtered it.
   Also note any complaint ("unsupported format") or proxying by BubbleUPnP.
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
5. **What does the control point decide by?** Only if test 1 gives `http://`: restart
   dms with `-snapcastFormat 'audio/flac;rate=44100;channels=2'` (and gmrender as
   before), play again. If that changes the result, the control point matches by MIME
   type; if not, it probably drops unknown protocols. Report either way.
6. **dms without a control point.** In `dlna/dms`, `just browse-root` (set `host` in the
   justfile to dms's address) shows the Snapcast `<res>` first for audio items, with the
   host the request used.

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
