# Testing in the devcontainer

The devcontainer has everything for building, `make check`, `make test-interop` and
`make test-aloop`. It has **no speakers and no audio server**: its only sound card is
the host's `snd-aloop` loopback card. Real listening tests belong on the host
([testing-host.md](testing-host.md)). Overview of all tests: [testing.md](testing.md).

## What the container provides

- Rust toolchain from `rust-toolchain.toml`, including `rust-analyzer` (for VS Code and
  for Claude Code's `rust-analyzer-lsp` plugin), clippy, rustfmt and CodeLLDB.
- GStreamer 1.26 dev packages and tools, `gstreamer1.0-alsa` (`alsasrc`/`alsasink`),
  `alsa-utils` (`aplay`, `arecord`, `amixer`), the PipeWire packages (not started).
- C++ `snapserver`/`snapclient` 0.31 (Debian), for interop tests only.
- `GST_PLUGIN_PATH=${workspaceFolder}/target/debug`, so freshly built plugins are found.
  The terminals get it, but agent tool shells may not (they don't get `containerEnv`): set
  it explicitly there.
- `--device=/dev/snd`: the host's sound devices, in particular the loopback card. Load it on
  the host first (`sudo modprobe snd-aloop`). On a host without `/dev/snd`, remove that line
  from `.devcontainer/devcontainer.json`.

## Sound in the container: ALSA on the loopback card

`postCreateCommand` installs [.devcontainer/asoundrc](../.devcontainer/asoundrc) as
`~/.asoundrc`. It makes ALSA's `default` device a **dmix on loopback subdevice 2**, so
`autoaudiosink` (it falls back to `alsasink` when no PulseAudio is running) and the C++
`snapclient` can play, several at once, without any audio server. Subdevices:

| Loopback subdevice | Used by |
|---|---|
| 0 | `make test-aloop` (its own dmix, defined by the test) |
| 2 | ALSA `default` in the container (`~/.asoundrc`) |
| others | free, e.g. `SINK="alsasink device=hw:Loopback,0,3"` |

You can't hear it, but you can record what was played and look at it:

```sh
make listen-all &                                   # runs until killed
arecord -D hw:Loopback,1,2 -f S16_LE -r 48000 -c 2 -d 10 /tmp/out.wav
kill %1
```

Format rule of snd-aloop: both ends of a subdevice must use the same format. The dmix
fixes S16_LE, 48 kHz, stereo, so record with exactly that.

## Gotchas

General ones are in [testing.md](testing.md#gotchas-both-environments).

| Symptom | Cause | Solution |
|---|---|---|
| `snapclient`: `Can't open default, error: Host is down` (and `listen-all` stops) | ALSA's `default` points to PipeWire (Debian's `/etc/alsa/conf.d/99-pipewire-default.conf`), and no PipeWire runs | `install -m 0644 .devcontainer/asoundrc ~/.asoundrc` (done by `postCreateCommand` on a rebuild) |
| `autoaudiosink`: `pulsesink … Failed to connect: Connection refused` (warning) | No PulseAudio server; it falls back to `alsasink` | Harmless with `~/.asoundrc` in place |
| `make listen-*` plays nothing audible | There are no speakers in the container | Expected. Record from `hw:Loopback,1,2` (above), or listen on the host |
| `arecord` from the loopback: `read error: Input/output error` or `File descriptor in bad state`, truncated WAV | The playing end closed or changed format; snd-aloop then breaks the capture | Start the capture first, use the dmix format (S16_LE/48000/2), use `-t raw` for analysis |
| `no 'Loopback' sound card` / `/dev/snd missing` | Module not loaded on the host, or container started before it was / without `--device=/dev/snd` | `sudo modprobe snd-aloop` on the host, then rebuild/restart the container. `make doctor` checks it |
| `/dev/snd present but not accessible` | Rootless Podman: the host user's ACLs on `/dev/snd/*` apply (`--userns=keep-id`) | Check `getfacl /dev/snd/*` on the host; possibly `--group-add keep-groups` in `runArgs` |
| Claude Code plugins (e.g. `rust-analyzer-lsp`), MCP servers or memory are gone after a rebuild | Before 2026-11-19, `~/.claude` and `~/.claude.json` lived in the container | Now on the named volume `claude-code-config-<id>`, with `CLAUDE_CONFIG_DIR=~/.claude` (so `.claude.json` is there too). To migrate an old container, see below |
| Claude Code says "permission denied" under `~/.claude` | The volume was created owned by root (mount point missing in an older image) | `sudo chown -R vscode: ~/.claude` |
| Claude Code's LSP plugin doesn't work (`Unknown binary 'rust-analyzer'`) | Only the rustup proxy existed, without the component | Now in `rust-toolchain.toml`; `rustup component add rust-analyzer` for an existing container |
| MCP servers (memory, xerj) unreachable | They run on the host and are reached via `host.docker.internal` | See `docs/claude/mcp-*.md` |

### Moving Claude Code's config onto the volume (once)

The volume is empty when it's created, so copy the old config out *before* the first
rebuild with the volume, and back in afterwards. The copy holds credentials and session
history: keep it out of git, and delete it afterwards.

```sh
# before the rebuild, in the old container, from the repo folder:
tar -C ~ -czf claude-migrate.tgz .claude .claude.json && chmod 600 claude-migrate.tgz
# after the rebuild, in the new container, from the repo folder:
tar -C ~ -xzf claude-migrate.tgz
mv ~/.claude.json ~/.claude/.claude.json      # CLAUDE_CONFIG_DIR expects it there
rm claude-migrate.tgz
```

Only the repo folder is mounted from the host. Everything else in the container,
including `/workspaces` itself, is lost on a rebuild. `claude-migrate.tgz` is in
`.gitignore`.

### PipeWire in the container (optional, not recommended)

ALSA is enough for everything above. If you need PipeWire anyway (e.g. to try `SRC=desktop`
or `pipewiresrc`), it can be started privately. Known problems:

- It needs a session D-Bus; realtime scheduling is unavailable (warnings, harmless).
- **WirePlumber finds no sound card**: without udev in the container, only the
  `auto_null` dummy sink exists. Add the loopback card by hand.
- **`pipewiresink` deadlocks on stop** (PipeWire 1.4.2; also with `audiotestsrc`). Use
  `pulsesink`/`autoaudiosink` via pipewire-pulse instead.
- While it runs, `~/.asoundrc`'s `default` is still the plain dmix. Use `pulsesink` or
  `pipewiresink` to go through PipeWire.

```sh
mv ~/.asoundrc ~/.asoundrc.off      # optional: let ALSA default go to PipeWire
dbus-run-session -- sh -c 'pipewire & wireplumber & pipewire-pulse & wait' &
sleep 3
pw-cli create-node adapter '{ factory.name=api.alsa.pcm.sink node.name=aloop-sink
    media.class=Audio/Sink api.alsa.path="hw:Loopback,0,3" audio.rate=48000
    audio.channels=2 audio.position=[FL FR] object.linger=true priority.session=2000 }'
wpctl status                        # the loopback sink should be the default
# record via PipeWire (snd-aloop's own capture end may not match PipeWire's format):
pw-record --target aloop-sink -P stream.capture.sink=true --format s16 /tmp/out.raw
```

Stop it with `pkill -x wireplumber; pkill -x pipewire-pulse; pkill -x pipewire`, and move
`~/.asoundrc` back.
