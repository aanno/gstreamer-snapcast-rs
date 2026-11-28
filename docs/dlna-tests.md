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

## Where we stand (2026-11-28)

With BubbleUPnP as the control point, on the host:

- **The whole chain works.**
  1. BubbleUPnP sends the item's ordinary dms URL to gmrender.
  2. gmrender asks dms with `Accept: audio/x-snapcast-tcp`.
  3. dms opens a session for the item (a `session-sender` helper) and answers with
     its `snapcast-tcp://` URI.
  4. `snapsrc` joins it.
  5. Pause, seek, volume and the end of the track go through the session.

  Tests 10, 11 and 12 passed.
- **Two renderers in one session** passed in the devcontainer (SOAP). On the host
  that's test 13.
- **A long run** is test 14 (`scripts/dlna-soak.sh` records it).
- **Doesn't work, by design:** offering the session as a second resource
  (`snapcast:` protocol or an unknown MIME type). BubbleUPnP never passes it on
  (tests 1 to 9). That's why the session is negotiated on the ordinary URL
  (`docs/dlna-snapcast-protocol.md`).
- **2026-11-28:** test 12 c and d passed on the host (40 minutes of an album). That
  run found dead streams piling up on the daemon, fixed the same day (stream linger).
- **Still open:** test 13 on the host, which needs two control points: BubbleUPnP
  follows only one renderer. Also test 14 overnight, with the fix. Both have their
  full command lines below.
- **Findings** so far, with the proposed solutions, are in the next section.

## Findings (2026-11-28)

What the host runs found besides pass or fail, and what's done about it. The
details are in the tests named.

1. **Dead streams piled up on the daemon. Fixed.**
   - **Seen:** in the CSV of the first long run (test 14), `dms_streams` grew by
     one per track played (5 → 9 in 15 minutes), while only 1–2 `session-sender`
     helpers ran. The daemon gained about one thread and 50 kB per stream.
   - **Not in dms:** dms's own numbers stayed flat (about 22 MB, 18 threads,
     20–23 files), and every helper exited on time. `dms_streams` counts the
     streams *on the Snapcast server* whose names start with `dms-`.
   - **The cause** was in the engine, by design since L1. A released stream stayed
     on its server until the server stopped, so that a restarting sender keeps
     its clients. dms gives every item a stream of its own, so the long-lived
     daemon collected one dead stream per track.
   - **The fix (engine, 2026-11-28):** a released stream is removed after a linger
     time, unless a sender takes it again meanwhile.
     - The default is 60 s.
     - The daemon decides, not the senders: `--stream-linger-secs` or
       `GST_SNAPCAST_DAEMON_STREAM_LINGER_SECS`. `none` keeps the old
       behaviour.
     - The daemon logs `Removed the released stream stream=…`.
     - Tested in the engine and the daemon, including that controllers no longer
       list the stream.
2. **The daemon: no verdict yet.** The leak hid any other growth in the daemon:
   its threads and memory rose with the stream count. The next night run (test 14)
   shows whether it stays flat once streams are removed.
3. **One BubbleUPnP can't keep several rooms playing.** It follows one renderer
   at a time, and switching to another renderer stops the one before (test 13,
   first host try). That's BubbleUPnP's design, not a fault of the PoC.
   - **For tests:** one control point per room, e.g. two BubbleUPnP instances on two
     devices, or BubbleUPnP and tutonaut. Each plays the same item from dms.
   - **For real use:** rooms join a session by playing the same item. Grouping
     rooms from one control point would need a control point that drives
     several renderers, or a Snapcast controller (Snapweb, Snapdroid) to move rooms
     between streams.
4. **tutonaut, a second control point, has known problems.** tutonaut is a Python/Tk
   UPnP control point (`dlna/tutonaut-upnp-control`, not under git yet). It is one
   of the few DLNA control points for the desktop. The user will set up a GitHub
   repository for it, so that its problems can be fixed there:
   - **It showed only one dms item** (the one playing on room 1), so room 2 couldn't
     play the same item. dms's browsing is unchanged from upstream, and dms served
     16 items. tutonaut browses with a plain `Browse(BrowseDirectChildren)` from
     object `0`, and nothing in it filters by what's playing. Its views are
     tailored to MiniDLNA (e.g. the "Albums" container). Next: browse dms from
     the root while nothing plays; compare with BubbleUPnP's view of dms.
   - **Play and Pause before any URI.** They produce errors on the renderer
     (`Playing failed (704)`, `Transition to PAUSE not allowed (701)`). These are
     harmless, but the buttons should be disabled until a track is set.
   - **Empty `CurrentURIMetaData`.** `SetAVTransportURI` sends no DIDL, so
     renderers know the title only from the stream's tags. For a session, the
     renderer reports the session's track anyway.
   - **Its queue had glitches on the first change** (2026-11-28, test 13, the same
     album queued as in BubbleUPnP). After that it worked: room 2 moved on with
     room 1. The glitch, from room 2's log:
     1. Track 04 ended at 11:28:38.
     2. 12 s later, tutonaut sent track 04 **again**. That restarted 04's session
        from the start.
     3. 8 s later it sent track 05, the right one.

     The restarted session 04 then played on unheard to its end (finding 5).
   - Its README says playback with this release wasn't tested.
5. **A session that every room left plays on, unheard.** Stop leaves the session;
   the others play on (by design). But when the last room leaves (Stop, or its
   control point switches to another item), the helper plays on to the end of the
   track, and only then starts its idle time. In the first run, track 09 played
   40 s past the switch to track 02.
   - **Harmless, but wasteful:** decoding and sending for nobody. The session also
     closes later: its idle time starts only at the end of the track.
   - **A solution:** the helper pauses when nobody listens.
     - It watches its stream on the Snapcast server through the JSON-RPC control
       port: `Server.GetStatus`, then the notifications (`Server.OnUpdate`,
       `Group.OnStreamChanged`, `Client.OnConnect`/`OnDisconnect`), or polling
       if snapcast-rs doesn't send all of them.
     - If no connected client is in a group playing its stream for a few seconds,
       it pauses. Then the idle time starts.
     - A room that plays the item again sends `play`, as now.
     - All of this is in `session-sender`; dms, gmrender and the protocol stay as
       they are. Another feeder needs the same logic, so `snapsink` could do it
       instead: it could report "no listeners" to its application (a bus message).
6. **gmrender sends its UPnP volume on every Play. Solution agreed, to be
   implemented later** (the user, 2026-11-28). On each new session (every
   track), gmrender sends its RenderingControl volume to the room's Snapcast client
   (`Session volume 100 %: sent`). A volume set in a Snapcast controller (Snapweb,
   Snapdroid) is lost at the next track. The two sides disagree, and UPnP always
   wins.
   - **The solution (later):** one volume per room, with the Snapcast server as
     the source.
     1. `snapsrc` reports the client's server volume and mute in its
        `snapcast-session` message. It knows them already, because it applies them.
     2. When joining a session, and whenever that volume changes, gmrender takes
        it as its UPnP volume. It updates `Volume`/`Mute` and sends `LastChange`,
        so control points show it.
     3. gmrender sends a volume to the server only when a control point calls
        `SetVolume` or `SetMute`, never on Play.

     Then a change from either side shows on both, and a new track keeps the
     room's volume. The Snapcast server keeps a client's volume while it runs
     (also across reconnects), so a room keeps its volume from one session to the
     next.
   - **Where:**
     - `snapsrc`: `volume` and `mute` fields in the `snapcast-session` message.
     - The gmrender fork: adopt them (`handle_session_message`), and stop sending in
       `output_gstreamer_play` and `set_player_uri`.
     - The protocol doc: a renderer takes the room's volume from the session.
   - **Until then:** each new track sets the room to the control point's volume. A
     simpler stopgap would be to send only on `SetVolume`/`SetMute`. The
     controller's volume would then survive, but BubbleUPnP's slider could show
     something else.
7. **A stopped room's position counted on. Fixed.** After Stop, gmrender went on
   extrapolating the session position (0:00:29, 0:00:30, … while STOPPED). Since
   2026-11-28 (gmrender fork), Stop forgets the session's state.

8. **A joined room doesn't follow to the next track.** BubbleUPnP played an album
   on room 1, and tutonaut joined room 2 into the same track: that worked well
   (2026-11-28, test 13). At the end of the track both rooms stopped, and only room
   1 went on.
   - **Why:** a session is one track. At its end, each renderer asks *its own*
     control point for the next one. BubbleUPnP has the album queued, so room 1
     goes on into the next track's session. tutonaut had nothing queued, so room 2
     stops. Plain DLNA control points don't know about other rooms.
   - **Workaround, no code: the same queue on both control points.** At the end of
     each track, both rooms ask for the same next item within a second or two. The
     first request opens its session, and the second joins it, as when joining by
     hand.
     - The catch: a skip or reorder on one control point splits the rooms.
     - **It works with BubbleUPnP and tutonaut** (2026-11-28, test 13), after a
       glitch in tutonaut's queue at the first change (finding 4). Two BubbleUPnP
       instances should work the same way; not tried yet.
   - **Solution, later: room 2 follows room 1.** It needs changes in dms and in
     gmrender:
     1. **dms:** when a renderer asks for the next item right after its session
        ended, dms moves that session's other listeners to the new session. It
        does that through the Snapcast server (`Group.SetStream`), the way
        Snapcast groups rooms anyway.
     2. **gmrender:** when its own control point has no next track, it doesn't
        leave at the end of a session. It waits a few seconds. If the server moves
        it to another stream, it plays along and reports the new track (metadata,
        duration, `PLAYING`) to its control point. Otherwise it stops, as now.
     - **Open rules:** who leads (the room whose control point picked the
       track). A room whose own control point then plays something else leaves
       the group. And what Stop on the leader does to the followers.
     - It fits the protocol: the session URI stays the same idea, and dms only
       uses the Snapcast server's own grouping.

9. **A room fell out of the session and silently played the file. Fixed.** In test
   13, room 1 was seeked, and room 2 didn't move: the rooms were out of sync.
   - **What happened** (track 08, 11:38):
     1. Room 2 asked first. dms started the helper, but the helper took **5 s** to
        be ready. Normally it takes well under a second. The file is on
        `~/mnt/musik`, perhaps a network mount that was slow to answer; not
        checked.
     2. gmrender gave up after 3 s (`GET request: … timeout`, then
        `offered a session, but answered ''`).
     3. gmrender then played the plain file, outside any session. dms logged
        `snapcast session not opened; serving the media` (`context canceled`).
     4. Room 1 asked 5 s later and got the session. From then on, the rooms played
        two different copies.
   - **The cause:** gmrender's 3 s timeout was shorter than dms's 10 s wait for
     its helper. The fallback to the media is right when there's no session at all.
     But here a session existed, and the room ended up outside it without a
     visible sign.
   - **The fix (gmrender fork, 2026-11-28):**
     - gmrender waits up to 15 s, longer than dms's 10 s.
     - The fallback now logs `playing the media itself, outside any session`.
     - The protocol doc now asks servers to answer within 10 s, and renderers to
       wait at least 15 s.
   - **To check after the next run:** how long the helpers take to be ready.
     Each `snapcast session opening` line in dms's log has its `Session ready` line;
     compare their times. If 5 s happens often, the file system is the suspect.

The numbered tests below keep their history. Their setup changed over time; the
current setup is above, and in tests 11 to 14.

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

13. **Two renderers, one session** (PoC step 5, since 2026-11-27; instructions
    rewritten 2026-11-28).
    - **Why two control points:** BubbleUPnP follows one renderer at a time.
      Switching it to the other renderer stops the first one (first host try,
      below). So each room gets its own control point:
      - **room 1:** BubbleUPnP;
      - **room 2:** a second BubbleUPnP instance (another phone or tablet), or
        tutonaut on the PC.

      Both must play **the same item from dms**. An item from another media
      server (the NAS) gives a plain room, not a second room in the session.
    - **Before you start** (repo root, host):

      ```sh
      make install                     # plugins and gst-snapcast-daemon into ~/.local
      make dlna-build                  # dms, gmrender, target/dlna/session-sender
      pgrep -a gst-snapcast-da         # an old daemon? Stop its senders, or:
      pkill -x gst-snapcast-da         # (only if nothing else uses it)
      mkdir -p target/dlna-logs
      ```

    - **Terminal 0, the daemon**, started by hand so that its log is at hand. It
      waits a day for senders instead of 5 s, and removes a released stream after
      60 s:

      ```sh
      gst-snapcast-daemon --grace-secs 86400 --stream-linger-secs 60 \
          --log target/dlna-logs/daemon.log
      ```

      Check its first log line: `Daemon listening … stream_linger=Some(60s)`. If
      there's no `stream_linger`, it's an old daemon (run `make install` again).
    - **Terminal 1, dms.** `MUSIC` must be the folder that holds your music. After
      changing it, browse dms again **from its root** in each control point: they
      may still show items of an earlier run, whose files dms doesn't have now.
      Playing such an item gives a 404 (`docs/testing-host.md`, gotchas):

      ```sh
      MUSIC=~/mnt/musik/Alice          # the folder with your music: check it!
      ls "$MUSIC" | head -3
      target/dlna/dms -path "$MUSIC" -friendlyName "dms snapcast PoC" \
          -snapcastStream 'snapcast-tcp://{host}:1704/{stream}?control=1705' \
          -snapcastFeeder "$PWD/target/dlna/session-sender --mode shared --idle-exit 120" \
          2>&1 | tee target/dlna-logs/dms.log
      ```

    - **Terminals 2 and 3, the rooms.** Each has its own name, UUID and port. Add
      `-I <interface>` if the wrong network is used (look for
      `Registered IPv4 <your LAN address>`):

      ```sh
      target/dlna/gmediarender -f "room 1" -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c01 -p 49500 \
          --snapcast-sessions --logfile stdout 2>&1 | tee target/dlna-logs/room1.log
      target/dlna/gmediarender -f "room 2" -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c02 -p 49501 \
          --snapcast-sessions --logfile stdout 2>&1 | tee target/dlna-logs/room2.log
      ```

      Both log `Asking servers for Snapcast sessions: Accept: audio/x-snapcast-tcp`
      at startup. Their `snapsrc` clients get instance numbers 1 and 2 by
      themselves.
    - **A sound output per room (optional).** By default, gmrender plays to
      GStreamer's default output. Each track opens a new audio stream, so an
      output picked in pwvucontrol (or pavucontrol) doesn't stick: the new stream
      goes to the default again. Instead, name the output on gmrender's command
      line:
      1. List the outputs (PipeWire, through its PulseAudio server):

         ```sh
         pactl list short sinks     # 2nd column: the name, e.g. alsa_output.usb-…analog-stereo
         ```

      2. Give each room its output with `--gstout-audiosink pulsesink
         --gstout-audiodevice <name>`. Or use `--gstout-audiopipe`, which takes
         a gst-launch description, to also name the room in pwvucontrol
         (`client-name`):

         ```sh
         target/dlna/gmediarender -f "room 1" -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c01 -p 49500 \
             --snapcast-sessions --logfile stdout \
             --gstout-audiopipe 'pulsesink device=<name 1> client-name="room 1"' \
             2>&1 | tee target/dlna-logs/room1.log
         target/dlna/gmediarender -f "room 2" -u 6a1c4e2f-5b3d-4c7a-9e10-2f8b6d4a1c02 -p 49501 \
             --snapcast-sessions --logfile stdout \
             --gstout-audiopipe 'pulsesink device=<name 2> client-name="room 2"' \
             2>&1 | tee target/dlna-logs/room2.log
         ```

      gmrender logs `Setting audio sink-pipeline to …` at startup. Use
      `pulsesink`, not `pipewiresink`, for two reasons:
      - `pipewiresink` hung on stop in the devcontainer (PipeWire 1.4.2, also with
        `audiotestsrc`; `docs/testing-devcontainer.md`, and a gotcha row in
        `docs/testing-host.md`). It's not verified with the host's PipeWire. gmrender
        stops its pipeline at every track, so a hang would stop the room.
      - `snapsrc` tunes the sink's sync properties (`adjust-sink`). `pulsesink` has
        the ones `alsasink` has, and `pipewiresink` hasn't
        (`docs/testing-host.md`). With two outputs, the rooms can't be
      compared by ear as one sound. Listen to each output, or put them side by
      side.
    - **Terminal 4 (optional), tutonaut** as the second control point:

      ```sh
      cd dlna/tutonaut-upnp-control && .venv/bin/python upnp_control.py
      ```

    - **Listening.** Both rooms play on the host's sound device. In sync you hear one
      clean sound; out of sync you hear an echo or a "phasing" sound. A second
      machine (gmrender with `snapsrc` installed) is the more realistic test, if
      there is one.
    - **Steps and what to check:**
      a. **Join.** Play a track on room 1 (control point 1). Then play the same
         track on room 2 (control point 2). dms logs **one**
         `snapcast session opening`, and two `snapcast session requested … GET`
         from the two rooms. Room 2 comes in at room 1's position.
      b. **Pause** on room 1: both rooms go quiet. Control point 2 shows "paused"
         too. Both rooms log `Session playing -> paused`.
      c. **Play and seek** on room 2: both rooms resume and jump. The helper logs
         `command setPosition {"position":…}` and `seek to …` (in dms's log).
      d. **Volume** on room 1: only room 1 changes (room 1 logs
         `Session volume N %: sent`).
      e. **Stop** on room 1: room 2 plays on. Room 1's position doesn't count on
         any more.
      f. **The end of the track:** both rooms log `Session playing -> stopped`.
         Each control point moves on in its own queue, so the rooms get their next
         sessions separately ("Findings" 8). With the same queue on both control
         points, they meet again in the next track's session.
      g. **Two sessions at once:** another track on room 1 while room 2 still plays
         the first. dms opens a second session, and you hear two different sounds.
      h. **Linger:** when the first session's helper has closed (dms logs
         `snapcast session closed`), the daemon logs
         `Removed the released stream stream=dms-…` about 60 s later.
    - **Done in the devcontainer** by hand (two gmrenders, SOAP, 2026-11-27): a to f
      passed. Both rooms were in one session with the same position. A pause in
      room 1 showed `PAUSED_PLAYBACK` in room 2, and a play and seek from room 2 moved
      both to 0:26. A volume in room 1 changed only room 1. After a Stop in room 1,
      room 2 played on. The end of the track stopped both. dms started one helper.
      Seen in passing: a stopped room still reports its last position (fixed
      2026-11-28).
    - **First host try, 2026-11-28: not possible with BubbleUPnP alone.**
      - BubbleUPnP follows one renderer at a time. Switching to the other renderer
        stops the first one, so two rooms can't be kept playing from it.
      - tutonaut controlled room 2, but played from the NAS's media server
        (MiniDLNA), not from dms: tutonaut showed only one dms item (see
        "Findings"). That doesn't test one session. It showed a session room next to
        a plain room, both working (room 2 asked the NAS, got the media and played
        it).
      - The startup errors in room 2's log (`Playing failed (704)`,
        `Transition to PAUSE not allowed`) were tutonaut's Play and Pause before any
        URI was set. They're harmless.
    - Result on the host: (open)

14. **The long run** (PoC step 5, since 2026-11-27; instructions rewritten
    2026-11-28): a night of playing, as for the daemon (`docs/testing-host.md`).
    - **Setup:** terminals 0 to 2 (and 3, if both rooms) as in test 13: the daemon
      by hand with `--stream-linger-secs 60`, dms with `--idle-exit 120`, and the
      logs in `target/dlna-logs/`.
    - **Terminal 5, the monitor** (a CSV row a minute; Ctrl-C stops it, and every
      row is complete):

      ```sh
      scripts/dlna-soak.sh target/dlna-logs/soak.csv
      ```

      Columns: the time, then `streams` and `dms_streams` (the daemon's streams,
      all and `dms-…`). Then, for each of dms, the daemon (`gst-snapcast-da`), the
      helpers (`session-sender`) and gmrender (`gmediarender`), the summed RSS (kB),
      threads, open files and the process count.
    - **In BubbleUPnP:** play a long queue on room 1, e.g. a few albums with
      "repeat all". Now and then join room 2 into the current track (second
      control point), pause, seek and stop.
    - **In the morning:**
      - **Sessions:** `dms_streams` stays at 1 to 3, close to `session-sender_count`.
        It may run up to one minute behind (the linger time), but it must not grow
        with every track. Each helper's `closed` in dms's log is followed by the
        daemon's `Removed the released stream` about 60 s later:

        ```sh
        grep -c 'snapcast session opening' target/dlna-logs/dms.log
        grep -c 'snapcast session closed' target/dlna-logs/dms.log
        grep -c 'Removed the released stream' target/dlna-logs/daemon.log
        ```

        The three counts match, up to the sessions still open.
      - **Memory, threads, files:** `*_rss_kb`, `*_threads` and `*_fds` stay flat for
        dms, the daemon and gmrender, and per helper (`session-sender_rss_kb /
        session-sender_count` stays about 17 MB).
      - **Logs:** no errors:

        ```sh
        grep -iE 'error|warn' target/dlna-logs/dms.log target/dlna-logs/daemon.log
        grep -E '^(ERROR|WARN)' target/dlna-logs/room1.log target/dlna-logs/room2.log
        ```

      - **Responsiveness:** pause, seek and volume still work at once.
      - Send the CSV and the logs (`target/dlna-logs/`).
    - **First run, 2026-11-28 (40 min, one room from dms):** see test 12, third
      host run. dms (about 22 MB, 18 threads), the helpers (about 17 MB each) and
      gmrender stayed flat. The daemon's stream count and threads grew with every
      track. That's fixed now (stream linger, see "Findings"). Repeat the night run
      with the fix.
    - Result on the host: (open)

12. **dms feeds a session per item** (PoC step 4, since 2026-11-27).
    - **Setup:**
      - `make install`: the plugins and the daemon (`~/.local/bin` must be on
        `PATH`).
      - `make dlna-build`: dms, gmrender and `target/dlna/session-sender`.
      - Terminal 0 (`make dlna-session`) isn't needed any more: dms starts the
        sessions.
      - Terminal 1, dms:

        ```sh
        target/dlna/dms -path ~/Music -friendlyName "dms snapcast PoC" \
            -snapcastStream 'snapcast-tcp://{host}:1704/{stream}?control=1705' \
            -snapcastFeeder "$PWD/target/dlna/session-sender --mode shared --idle-exit 120"
        ```

      - Terminal 2: gmrender as in the setup above (`--snapcast-sessions`).
    - **Expected:**
      a. **The picked track plays now**, not a stand-in. dms logs
         `snapcast session opening stream=dms-…`, the helper's `Session ready`, then
         `snapcast session requested … method=GET`.
      b. **Pause, seek, volume, the end of the track and Stop** behave as in test 11.
         At the end of a track, BubbleUPnP's next track opens the next session.
      c. **Another track** gets another `dms-…` stream. Play the same track again
         within the idle time: no new `opening`, the session is reused.
      d. **Idle close:** 120 s after a session stops playing, dms logs
         `idle: … closing it` and `snapcast session closed`.
      e. **Snapweb** (`http://<host>:1780`, if the daemon serves it) shows one stream
         per open session, named `dms-…`, with the track's title, position and length.
    - **Done in the devcontainer** by hand (SOAP, 2026-11-27): a to d passed. Also, a
      second room joined the same session, and pause in one room paused both.
    - **Result on the host, 2026-11-27: passed** (a, b; one item). dms from
      `dlna/dms` with `-snapcastFeeder ../../target/debug/examples/session-sender`
      (no `--mode`, so the helper ran an embedded server; fine for one session).
      gmrender: session `dms-2b5de2b3edd9` started paused, and its play started it.
      Four seeks, one volume send per change, pause and resume, and the end of the
      track (STOPPED) all worked. Not yet tried: several items or rooms at once (needs
      `--mode shared`), and the idle close.
    - **Second host run, 2026-11-27, with `--mode shared --idle-exit 120`: passed** (a,
      b; one item).
      - dms opened the session on gmrender's GET and had it ready within the second
        (`Session ready`). There was no fallback warning, so the daemon served it.
      - Pause and resume (twice), a seek to 25 s, volume changes (one send each) and
        the end of the track (gmrender STOPPED) all worked.
      - dms was stopped before the 120 s idle close.
      - Still open: c (several items) and d (the idle close).
    - **Third host run, 2026-11-28 (with test 14's monitor): c and d passed.**
      BubbleUPnP played the album `Il Sole Nella Pioggia` (dms `-path` its artist's
      folder, 16 items) on room 1 for 40 minutes, with
      `--mode shared --idle-exit 120`. Logs: `dms-test.log`, `gmr-room1.log`,
      `gmr-room2.log`, CSV `target/dlna-soak-20261128-1011.csv`.
      - c: every track got its own `dms-…` stream. Each session opened within the
        second and was ready before gmrender's GET. A track played again later
        (02, 03) opened a new helper on the same stream name.
        A second Play on the same item (BubbleUPnP re-sends the URI after a
        renderer switch) reused the running session.
      - d: every session closed 120 s after its end (`idle: … closing it`, then
        `closed … exit=<nil>`). Only 1 or 2 helpers ran at a time.
      - Also passed: pause and resume, a seek to 2:42, and the next track
        following each track's end within about 2 s.
      - **Found: dead streams piled up on the daemon.** The CSV showed `dms_streams`
        growing by one per track (5 → 9 in 15 minutes) while only 1–2 helpers ran,
        with about one daemon thread more per stream. A released stream stayed on
        the server until the server stopped, by design since L1. **Fixed
        2026-11-28:** a released stream is removed after a linger time (default
        60 s, daemon `--stream-linger-secs`; `docs/shared-server.md`).
      - **Found: a stopped room's position counted on.** After Stop, gmrender
        went on extrapolating the session position (0:00:29, 0:00:30, … while
        STOPPED). **Fixed 2026-11-28:** Stop forgets the session's state.
      - **Seen, not changed:**
        - A session that every room left (Stop, or the control point switched to
          another item) plays on unheard to its end, then idles. Here, track 09
          played 40 s past the switch to track 02. It's harmless, but it uses the
          idle time. A helper could pause when no client listens.
        - gmrender sends its UPnP volume (100 %) on every Play. So a volume set
          in a Snapcast controller is overwritten by the next track.
    - Found on the way:
      - `make dlna-build` on the host failed after a devcontainer build: the shared
        `target/dlna/gmrender-build` kept dependency files with `/workspaces/…`
        paths (`No rule to make target …/main.c`). Fixed: the target wipes a build
        tree that belongs to another checkout path (`docs/testing-host.md`).
      - `-snapcastFeeder ../session-sender --mode shared …` without quotes made dms
        reject `-mode`: the helper's options must be quoted with it (dms's help now
        says so; `dlna/dms/README-snapcast.md`).
      - The `auto` fallback is now a visible warning, and a too-long
        `XDG_RUNTIME_DIR` is an error (`docs/testing-host.md`, gotchas).

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
        with the C++ client also differ at the same percent. **Decided (the user,
        2026-11-27): snapsrc uses the C++ curve**, done the same day: 50 % ≈ −12 dB,
        10 % ≈ −31 dB.
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
