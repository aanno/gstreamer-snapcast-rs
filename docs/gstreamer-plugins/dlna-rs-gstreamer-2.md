# DLNA App Entwicklung (1)

**Exported with:** [AI Chat Exporter](https://ace.covai.org)  
**Source:** ChatGPT  
**Date:** 7.11.2026, 15:57:53  
**Link:** [https://chatgpt.com/c/6aef37d6-1990-83eb-9186-63d3e953e033](https://chatgpt.com/c/6aef37d6-1990-83eb-9186-63d3e953e033)  
**Model:** auto  
**Method:** API  

## Response:
Agreed. That is a more coherent architecture than treating Snapserver as an external process.

The key idea is to make **Snapserver an embedded audio-distribution engine**, with GStreamer providing the media abstraction on its input side.

This is also consistent with Snapcast's existing model: multiple independent sources can feed the server, and clients can be assigned to streams and groups. <Cite refs={["turn404565search0","turn404565search2"]}/>

## Proposed architecture

<box border radius="lg" padding={3} gap={2}>
  <box background="rgba(100,160,220,0.10)" radius="md" padding={3} align="center">
    **GStreamer sender pipeline(s)**

    `pipewiresrc` · `filesrc` · `appsrc` · other sources
  </box>
  <box align="center"><icon name="arrow-down" color="secondary" /></box>
  <box background="rgba(100,160,220,0.10)" radius="md" padding={3} align="center">
    **Snapcast GStreamer sink(s)**

    One sink per logical stream
  </box>
  <box align="center"><icon name="arrow-down" color="secondary" /></box>
  <box border={{size:1,color:"#3984B8"}} radius="md" padding={3} gap={2}>
    **Embedded Snapserver library**
    <grid columns={2} gap={2}>
      <grid-item background="surface-secondary" radius="md" padding={2} align="center">Stream A</grid-item>
      <grid-item background="surface-secondary" radius="md" padding={2} align="center">Stream B</grid-item>
      <grid-item background="surface-secondary" radius="md" padding={2} align="center">Stream C</grid-item>
      <grid-item background="surface-secondary" radius="md" padding={2} align="center">Stream N</grid-item>
    </grid>
    <text color="secondary" size="xs" textAlign="center">Client connections · groups · synchronization · encoding · JSON-RPC control</text>
  </box>
  <box align="center"><icon name="arrow-down" color="secondary" /></box>
  <box background="rgba(100,160,220,0.10)" radius="md" padding={3} align="center">
    **Snapcast TCP clients**

    GStreamer receiver plugins in DMRs
  </box>
</box>

The GStreamer abstraction would then be independent of whether the sender source is PipeWire, Android capture, a file, or an application-generated PCM stream.

## 2. One important implementation caveat

The current Snapcast project is a C++ application with a server executable, not an established public embeddable server-library API. Its existing source architecture would need some refactoring to make that cleanly possible. <Cite refs={["turn120352view0"]}/>

I would aim for three layers:

1. **Snapserver core library** — streams, clients, groups, synchronization, encoding and protocol.
2. **Server runtime** — TCP listeners, JSON-RPC, configuration and lifecycle.
3. **GStreamer integration** — a `GstBaseSink`-derived element that submits PCM to a stream in the embedded server.

The separation between core and runtime matters. A GStreamer plugin should not have to start a server process or own its global lifecycle.

## 3. Multiple GStreamer sinks

I would expose something conceptually like:

```text
snapserver_create()
snapserver_add_stream(server, "music")
snapserver_add_stream(server, "desktop")
snapserver_start(server)

snapsink(stream="music")
snapsink(stream="desktop")
```

Each sink submits timestamped PCM buffers to its assigned stream. The server handles distribution to clients and groups.

One subtlety: **multiple sinks do not necessarily mean multiple independent clocks**. If pipelines have different clocks, the library needs a well-defined policy for mapping their timestamps onto the server timeline. This is particularly important for live capture.

## 4. A potentially better alternative

I found an independently developed Rust crate, <Link ref="turn404565search3" title="snapcast-server"/>. It explicitly describes itself as an embeddable synchronized multiroom server with a channel-based API.

Worth evaluating, but I would not assume protocol or feature parity with the official C++ implementation without testing.

## My recommendation

Keep the official Snapcast implementation as the reference and first investigate extracting its server core into a shared library. That avoids maintaining a second implementation of the protocol and synchronization machinery.

The resulting architecture would be:

- GStreamer plugin = media-facing API.
- Snapserver library = distribution engine.
- DMC = lifecycle and orchestration.
- gmrender = UPnP integration.

That is a strong architectural boundary, and it also leaves room for a future Android sender without changing the distribution model.

## Prompt:
Could https://github.com/metaneutrons/snapcast-rs help for implementing the 2 gstreamer plugins? I think rust is supported for these plugins!

## Response:
<think>
**3 Websites durchsucht**

11s nachgedacht
</think>

**Yes. `snapcast-rs` looks highly relevant—in fact, it could eliminate most of the custom Snapcast protocol implementation on both sides.** I would now consider it the primary implementation candidate for the two GStreamer plugins, rather than extracting the C++ Snapserver into a library.

The important discovery is that it already separates the Snapcast protocol engines from the application shell:

- `snapcast-server`: embeddable server engine, including stream encoding, client connections and synchronization.
- `snapcast-client`: embeddable client engine, including protocol handling, decoding and clock synchronization.
- `snapcast-proto`: shared protocol definitions.

Both engines expose Rust APIs and communicate through typed channels. The project explicitly targets embedding without launching the Snapcast executables. <Cite refs={["turn779588search0"]}/>

GStreamer also has mature Rust bindings, including APIs for implementing plugins and custom source/sink elements. <Cite refs={["turn779588search1","turn779588search2","turn779588search3"]}/>

## 1. Mapping the crates to the two plugins

I would implement both elements in Rust, sharing the Snapcast libraries directly.

| GStreamer element | Snapcast dependency | GStreamer base class | Responsibility |
|---|---|---|---|
| `snapsink` | `snapcast-server` | `GstBaseSink` | Submit timestamped PCM to a server stream |
| `snapsrc` | `snapcast-client` | `GstPushSrc` | Receive decoded PCM and deliver it downstream |

The mapping is unusually clean.

### Sender: `snapsink`

The server API already provides the relevant abstraction:

```rust
let (mut server, events) = SnapServer::new(config);

let music_tx = server.add_stream("music");
let desktop_tx = server.add_stream("desktop");
```

Each stream gets its own encoder. The GStreamer sink would convert incoming buffers into the library's `AudioFrame` representation and submit them to the appropriate stream channel.

Crucially, I would use **one shared SnapServer instance for all `snapsink` elements**, not one instance per element.

### Receiver: `snapsrc`

The client API is similarly useful:

```rust
let (mut client, events, mut audio_rx) = SnapClient::new(config);
```

The plugin can consume decoded PCM frames and expose them as GStreamer raw audio buffers.

There is an important detail here: the client also exposes a shared time-synchronized PCM buffer and a `time_provider`. I would investigate those before settling on `audio_rx` as the actual playback interface. The latter is explicitly documented as an audio-monitoring interface; the former may be more appropriate for implementing clock-aware playback. <Cite refs={["turn779588search0"]}/>

## 2. What snapcast-rs does not provide

There are several limitations worth accounting for.

| Area | Current situation | Consequence |
|---|---|---|
| Dynamic streams | Runtime stream addition is application-owned and currently unsupported by the library | Create streams before starting the server, or extend the API |
| JSON-RPC | Implemented in the binary, not the server library | DMC needs a separate control implementation |
| HTTP / Snapweb | Binary-only | Not needed for the GStreamer elements |
| Network listener | Supplied by the embedding application | Manage TCP listener lifecycle yourself |
| API stability | Pre-1.0 | Pin versions and expect API changes |

The dynamic-stream limitation is particularly relevant to your multiple-sinks requirement. Static stream creation at initialization is sufficient for a first prototype, but I would eventually want a proper `add_stream`/`remove_stream` runtime API.

<Cite refs={["turn664472view0"]}/>

## 3. Compatibility and licensing

Two things deserve explicit attention:

- **Codec compatibility:** Use PCM or FLAC for interoperability with the original C++ Snapcast clients. The Rust-specific `f32lz4` codec is not compatible with them.
- **License:** `snapcast-rs` is GPL-3.0-only. This has implications for distributing a GStreamer plugin that links against it. Resolve that before choosing the dependency for a product intended to have a different license.

<Cite refs={["turn664472view0"]}/>

## 4. Proposed project structure

I would organize the implementation approximately like this:

```text
gst-snapcast/
  snapcast-engine/
    server.rs
    client.rs
    streams.rs
    clock.rs

  gst-plugin-snapcast/
    plugin.rs
    snapsink.rs
    snapsrc.rs
```

The engine layer should know nothing about GStreamer. It owns Snapcast instances, their asynchronous runtimes, stream management and lifecycle.

The plugin layer translates between GStreamer buffers and the engine APIs.

This also gives us a useful separation for Android: the Snapcast engine can be reused without requiring GStreamer to own the entire application architecture.

## 5. Recommendation

<badge color="success">Strong candidate</badge>

I would prefer `snapcast-rs` over extracting the C++ Snapserver, subject to validating its maturity and licensing.

It potentially gives us:
- A Rust-native sender and receiver implementation.
- No custom Snapcast wire-protocol implementation.
- Shared code between Linux and Android.
- A natural integration with Rust GStreamer plugins.
- Independent control over Snapcast lifecycle and GStreamer lifecycle.

**The next experiment I would make is a minimal `snapsink ! Snapserver ! snapsrc` loopback pipeline**, using PCM first. That tests the critical integration boundary—including buffer timing and clock synchronization—before involving PipeWire, UPnP or gmrender.

