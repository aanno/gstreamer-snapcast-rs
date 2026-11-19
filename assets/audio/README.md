# Test audio

Real music for listening tests (`scripts/host-play.sh`, `make listen-send SRC=file:…`).
Automated tests generate their signals instead (sine, ticks), because those can be
checked sample by sample.

| File | Title, author | Licence | Format |
|---|---|---|---|
| `freejazz_medley.flac` | "FreeJazz Medley Loop" by sauer2 | [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) | FLAC, 16 bit, stereo, 44.1 kHz, 38 s, 2.3 MB |

Source: <https://opengameart.org/node/3655> (downloaded 2026-11-19 from
`https://opengameart.org/sites/default/files/freejazz_medley.flac`).

SHA-256 `37c8e8725be3bc002a0e62e743b8b093e374ec1d6fbb221801387322a028a0cf`.

Why this file: drums and transients make gaps, wobble and pitch changes easy to hear,
and 44.1 kHz also exercises resampling to the 48 kHz stream format.
