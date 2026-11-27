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

**Since 2026-11-27 (content negotiation, [dlna-snapcast-protocol.md](dlna-snapcast-protocol.md)).**
The commands of tests 1–9 below are the old ones (`--snapcast-formats`, `/snapcast`
URLs, `snapcast://`). They are kept for the record but don't work with the current forks.

After pulling, gmrender's build needs regenerating (`configure.ac` changed: it now
needs `gio-2.0`): `make dlna-build` handles that in `target/dlna`. For a build by hand,
run `./autogen.sh && ./configure … && make && make install` again.

The renderer plays sessions with `snapsrc`, so **install the plugins** on the host
(`make install`, per user). Without them, gmrender logs a warning at startup and only
plays media.

Terminal 0, the Snapcast server with a stream to join. For now it stands in for dms
(dms doesn't feed sessions until PoC step 4). You'll hear this, not the track you
picked:

```sh
CONTROL=1 STREAM=default SRC=file:assets/audio/freejazz_medley.flac make listen-send
```

Terminal 1, the media server. Asked for an audio item with
`Accept: audio/x-snapcast-tcp`, it answers with the session URI instead of the media,
and logs `snapcast session requested`. All other requests get the media, as before:

```sh
target/dlna/dms -path ~/Music -friendlyName "dms snapcast PoC" \
    -snapcastStream 'snapcast-tcp://{host}:1704/default?control=1705'
```

Terminal 2, the renderer. `--snapcast-sessions` makes it ask before playing an
`http://` URI. `-I` is the network interface for UPnP, and `-p` fixes its HTTP port:

```sh
target/dlna/gmediarender -f "gmrender snapcast PoC" -I eno1 --logfile stdout \
    -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c01 -p 49500 \
    --snapcast-sessions 2>&1 | grep --line-buffered -E 'AVTransportURI|snapcast|Set uri|Registered'
```

At startup, gmrender logs `Asking servers for Snapcast sessions: Accept: audio/x-snapcast-tcp`
(or the warning, if `snapsrc` isn't found).

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

## Where we stand (2026-11-27, evening)

In short, with BubbleUPnP as the control point:

- **Works:**
  - Discovery of the PoC gmrender (installed, own UUID).
  - Playing over HTTP, with or without the Snapcast extras.
  - **The `http-get` wrapper reaches gmrender** (test 9a and 9b, 2026-11-27).
    BubbleUPnP sends dms's `/snapcast?path=…` URL to gmrender, and dms answers
    gmrender with the `snapcast://` URI.
- **Does not work:** a session resource with protocol `snapcast:` (test 1), or with
  an own MIME type `audio/x-snapcast` (test 8, confirmed 2026-11-27). BubbleUPnP
  silently sends nothing. Only types it knows (`audio/flac`, `audio/x-scpls`) pass.
- **No sound yet, as expected:** gmrender hands the wrapper URL straight to `playbin`
  (`Missing decoder: text/uri-list`).
- **Built since then (PoC step 2, 2026-11-27):** content negotiation instead of a
  second resource. gmrender asks dms for the item's ordinary URL with
  `Accept: audio/x-snapcast-tcp`, and dms answers with a `snapcast-tcp://` URI.
  `snapsrc` plays it and joins the stream. Tested by hand in the devcontainer.
- **Test 10 passed on the host (2026-11-27):** BubbleUPnP → gmrender → dms
  negotiation → `snapsrc` plays the Snapcast stream. PoC step 2 is done for real.
- **Built (PoC step 3, 2026-11-27):** gmrender passes Pause, Seek, position,
  volume and the end of a track to the session (snapsrc's `snapcast-session`
  message and action signals). `make dlna-session` stands in for dms's feeder.
- **Next:** test 11 on the host. Then step 4: dms feeds a session per item, behind a
  `SessionFeeder` interface ([dlna.md](dlna.md)).


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
   **Repeated 2026-11-27, renderer selected: failed again, so it's confirmed.** Scripts
   and logs: `dms-c-xsnapcast.*`, `gmr-c-xsnapcast.*`. gmrender got four `Pause`
   actions from BubbleUPnP (refused with 701, nothing was playing). So BubbleUPnP was
   controlling gmrender, but it never sent `SetAVTransportURI`, and dms saw no request,
   not even BubbleUPnP's probe. BubbleUPnP refuses MIME types it doesn't know. (No
   `Subscription request` was logged this time, though actions arrived. The subscription
   check is a hint, not proof.)

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

   Results 2026-11-25: **a: nothing plays, no `SetAVTransportURI`**; the log excerpt
   shows no subscription from BubbleUPnP either, so it should be repeated with the
   renderer surely selected. b: first attempt failed on the command line (dms needs
   `-snapcastStream` and a space before `-snapcastFormat`). **b, second attempt:**
   no sound, BubbleUPnP shows "playing"; dms logged `snapcast session requested` 7×
   within a second (every track of the album) from the **phone's** address, while
   gmrender's log has no subscription after its restart: BubbleUPnP was playing on
   the phone (local renderer), not on gmrender, and its queue preloaded the session
   URLs. So: with `audio/flac`, BubbleUPnP uses the resource; and a client unaware of
   Snapcast gets the one-line answer instead of audio, which content negotiation would
   avoid. Repeat a and b with the renderer re-selected (below).

   **After restarting gmrender, re-select it in BubbleUPnP** and check for
   `Subscription request` in gmrender's log before playing: otherwise BubbleUPnP may
   play on the phone.

   **Results 2026-11-27 (repeated, renderer re-selected): a and b both passed.** No
   sound in either, but that is expected before PoC step 2. Scripts and logs:
   `dlna/dms/dms-{a-scpls,b-flac}.{sh,log}`, `dlna/gmrender-resurrect/gmr-{a-scpls,b-flac}.{sh,log}`.
   - gmrender got `SetAVTransportURI` with dms's wrapper URL
     (`http://[<host IPv6>]:1338/snapcast?path=freejazz_medley.flac`). BubbleUPnP's DIDL
     carried only that `<res>`, with our MIME type and its own 4th field
     (`DLNA.ORG_FLAGS=…;DLNA.ORG_OP=01`).
   - dms logged `snapcast session requested` from gmrender's address (the host,
     `…7285:c2ff:fea4:1b9f`) and answered with the IPv4 URI
     `snapcast://192.168.10.139:1704/default?control=1705`. Before that, the phone
     (`…3471:…`) also fetched the URL once or twice: BubbleUPnP probes the resource
     before sending it to the renderer.
   - gmrender handed the URL to `playbin`, which typefound the one-line answer as
     `text/uri-list`: `Missing decoder: text/uri-list`, then `not-linked`. That's the
     expected error from test 8. gmrender still reported `PLAYING`, so BubbleUPnP
     showed "playing".
   - In run a, the phone also fetched `/snapcast?path=go.mod`: dms takes `.mod` for a
     tracker module (audio), so `go.mod` gets a session resource. That's harmless; serve
     a folder with only music.

   Conclusions:
   - BubbleUPnP passes on a session resource of a MIME type that both sides list. The
     earlier failures of 9a and 9b came from the renderer not being selected.
   - Test 8 (`audio/x-snapcast`) may have failed for the same reason. Repeat it with
     the renderer re-selected. If it passes, keep the own type: it's the cleanest.
     (Repeated 2026-11-27: it fails. BubbleUPnP refuses the unknown type; see test 8.)
   - Next is PoC step 2. gmrender recognises the session resource by the protocolInfo
     in the metadata (one of its `--snapcast-formats`), fetches the wrapper URL itself,
     and gives `playbin` the `snapcast://` URI. `snapsrc` needs a `snapcast://` URI
     handler for that.

Later steps get their tests here when they are built: `playbin uri=snapcast://…`
(PoC step 2), gmrender playing it with Pause/Seek going to the session (step 3), dms
feeding a session with `go-gst` (step 4), two renderers in sync (step 5).

11. **The session follows the control point** (PoC step 3, since 2026-11-27).
    - **Setup:**
      - **Reinstall the plugins** (`make install`): `snapsrc` changed.
      - **Rebuild gmrender** (`make dlna-build`, or by hand as before).
      - **Terminal 0** is now the stand-in sender, which obeys play, pause and seek
        and ends with the file: `make dlna-session SESSION_FILE=<a music file>`
        (default `assets/audio/freejazz_medley.flac`, 38 s; ports 1704/1705,
        stream `default`).
      - **Terminals 1 and 2:** dms and gmrender as in the setup above.
      - **Optional:** `dlna-session` serves no Snapweb (no HTTP port). A controller
        app on the JSON-RPC port 1705 (e.g. Snapdroid) shows the room's volume and
        can pause the session too.
    - **In BubbleUPnP**, play any track from dms on gmrender, then:
      a. **Position:** BubbleUPnP's progress bar shows the session's position and its
         length (38 s for the default file), not the picked track's.
      b. **Pause and resume** in BubbleUPnP: the sound stops after up to a second
         (the Snapcast buffer) and comes back. Terminal 0 logs `command pause` /
         `command play`; gmrender logs `Session playing -> paused`.
      c. **Seek** in BubbleUPnP (drag the bar): terminal 0 logs `command setPosition`
         and `seek to …`. The sound jumps after up to a second.
      d. **Volume** in BubbleUPnP: gmrender's Snapcast client changes volume
         (`set-server-volume`). A Snapcast controller shows the same number.
      e. **End of the track:** at the end of the file, gmrender logs
         `Session playing -> stopped` and `Session ended`, and BubbleUPnP goes on to
         its next track. That track plays the same session from the start again (the
         stand-in has only one file).
      f. **Stop** in BubbleUPnP: gmrender leaves the session; terminal 0 goes on.
    - **Done in the devcontainer** by hand (SOAP requests, 2026-11-27): a to f passed,
      including a pause from a Snapcast controller (`Stream.Control pause` over
      JSON-RPC). gmrender's transport went to `PAUSED_PLAYBACK` and back.
    - **Result on the host, 2026-11-27, first run:**
      - b (pause and resume) passed.
      - c (seek) failed: every seek started the track from the beginning.
        gmrender logged `setPosition {"position":20,000}`, and the sender received
        `setPosition null`. The cause was gmrender formatting the number with the
        host's German locale (decimal comma), which isn't JSON. snapsrc then sent
        `null`.
      - Fixed the same day: gmrender `g_ascii_formatd`, and snapsrc now refuses
        parameters that aren't JSON. Checked in the devcontainer (C locale only):
        a seek to 0:00:20 arrived as `{"position":20.0}`.
      - **Repeat c** after `make install` and `make dlna-build`. Then a, d, e and f.
    - **Second run on the host, 2026-11-27:**
      - c (seek) passed: `setPosition {"position":25.0}` and others arrived, and the
        sound jumped.
      - e (end of the track) passed: `stopped`, then BubbleUPnP's next track
        restarted the session.
      - b passed again.
      - d (volume): unclear by ear. The log shows the volume changes reaching
        gmrender's session branch while the session played, but gmrender didn't log
        what it sent (it does now: `Session volume N %: sent`).
      - Likely reason it was hard to hear: snapsrc applies the percent linearly, so
        40 % is only −8 dB. The C++ snapclient's default software mixer uses
        `(10^v − 1) / 9` (40 % ≈ −15 dB, 10 % ≈ −30 dB). So rooms with snapsrc and
        with the C++ client also differ at the same percent. To decide: snapsrc
        follows the C++ curve.
      - Repeat d with a Snapcast controller app (e.g. Snapdroid on port 1705)
        showing the room's volume.

10. **Content negotiation with BubbleUPnP** (PoC step 2, since 2026-11-27). Setup above
    (terminals 0–2), gmrender re-selected in BubbleUPnP after each restart. Browse
    dms, play a track on gmrender.
    - **Expected:** you hear terminal 0's audio, not the track (dms doesn't feed
      sessions yet).
      - gmrender logs `Session snapcast-tcp://<host IPv4>:1704/default?control=1705 for http://…/res?path=…`,
        then `Set uri to 'snapcast-tcp://…'`.
      - dms logs `snapcast session requested` twice from gmrender's address
        (`method=HEAD`, then `GET`).
      - BubbleUPnP's own probe of the URL has no such `Accept`, so it gets the
        media.
    - **Check the fallback:** restart gmrender without `--snapcast-sessions`. The track
      itself plays.
    - **Check the second fallback:** restart dms without `-snapcastStream`. The track
      itself plays, with sessions on in gmrender.
    - **Check the transport choice (optional):** give dms
      `-snapcastStream 'snapcast-ws://{host}:1780/default'` before the tcp one. gmrender
      still gets the tcp URI, because snapsrc plays only `snapcast-tcp`.
    - Done in the devcontainer by hand (SOAP requests in place of BubbleUPnP,
      2026-11-27): all of the above passed, and snapsrc joined the stream named in
      the URI.
    - **Result on the host, 2026-11-27: passed.** Ticks from `make listen-send` (its
      default source) came out of gmrender, picked in BubbleUPnP as
      `freejazz_medley.flac`. Scripts and logs: `dlna/dms/dms-test.log`,
      `dlna/gmrender-resurrect/gmr-test.log`.
      - BubbleUPnP sent the ordinary `/res` URL (`http-get:*:audio/flac:…`,
        over IPv4 this time).
      - gmrender: `Asking servers … Accept: audio/x-snapcast-tcp` at startup. On
        play: `Session snapcast-tcp://192.168.10.139:1704/default?control=1705`,
        then `Set uri to 'snapcast-tcp://…'` and `PLAYING`.
      - dms: `HEAD`, then `GET`, from gmrender's address, 22 ms in all.
      - Seen on the way, all for step 3:
        - The position gmrender reports (`RelativeTimePosition`) is its local
          pipeline's: it counts up from 0.
        - The source is live, so the track never ends. BubbleUPnP's 38 s item
          plays on, and the next item doesn't follow by itself.
      - Fallback checks and the transport choice weren't run on the host; they
        passed in the devcontainer.

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
