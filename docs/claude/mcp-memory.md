# memory MCP server: usage and known problems

`memory` is [doobidoo/mcp-memory-service](https://github.com/doobidoo/mcp-memory-service).
It is a persistent, semantically searchable memory shared by agent sessions. These notes
come from a hands-on test (2026-11-19, server version 10.70.3).

## Setup

- It runs on the **host** as a Podman compose service (`ghcr.io/doobidoo/mcp-memory-service`),
  with streamable HTTP on port 8765. Claude Code connects to
  `http://host.docker.internal:8765/mcp` (`claude mcp add`, see `scripts/setup.sh`).
- Storage: the `sqlite_vec` backend, with embeddings from `all-MiniLM-L6-v2` computed
  locally in the container. The data lives on the named volume `doobidoo-data`, mounted at `/data`.
- **`MCP_MEMORY_SQLITE_PATH` must point into the volume.** Without it, the server uses
  `/app/sqlite_db`, which it can't open. Every storage call then fails with
  `Failed to initialize SQLite-vec storage: unable to open database file`, while
  `memory_stats` still answers. Working setting:

  ```yaml
  environment:
    - MCP_MEMORY_STORAGE_BACKEND=sqlite_vec
    - MCP_MEMORY_SQLITE_PATH=/data/sqlite_vec.db
    - MCP_MEMORY_SQLITE_PRAGMAS=journal_mode=WAL,busy_timeout=15000
  volumes:
    - doobidoo-data:/data:z
  ```

  Environment changes need `podman compose down && podman compose up -d`. A restart is not enough.
- After the server restarts, Claude Code loses the connection (`ECONNREFUSED` / `ECONNRESET`).
  Reconnect with `/mcp`.

## Project rule: tags

**Every memory written from this project must carry at least one of the tags
`gstreamer` or `snapcast`.** The store is shared with other projects, and these tags
are how this project's memories are found and cleaned up. Add more specific tags as
needed (e.g. `snapsrc`, `timing`, `interop`). Mark throwaway entries `temporary` and
delete them afterwards.

```json
{"content": "…", "metadata": {"tags": "gstreamer,snapcast,timing", "type": "decision"}}
```

When searching, filter with `tags: ["gstreamer","snapcast"]` (`tag_match: "any"`) to stay
within this project.

## Tools

Tested and working:

- `memory_health`: database status, memory count and size. Call it first: it is the
  only tool that shows storage errors clearly.
- `memory_store`: content plus `metadata.tags` (a comma-separated string or an array) and
  `metadata.type`. Returns a content hash.
- `memory_search`: semantic by default. Also supports `exact`, `hybrid` and `ranked`,
  plus tag and time filters (`time_expr: "last week"`, `after`/`before`). A paraphrased
  query found the test memory.
- `memory_delete`: by `content_hash`, by tags or by time range. Use `dry_run: true` before
  deleting in bulk. Deletion works, unlike in xerj.
- `memory_stats`: cache statistics and backend info (`sqlite_path`). Useful to see which
  database path is in use.

Present but untested: `memory_list`, `memory_update`, `memory_cleanup`,
`memory_conflicts` / `memory_resolve`, `memory_graph`, `memory_harvest`, `memory_ingest`,
`memory_observe`, `memory_quality`, `memory_store_session`, `mistake_note_*`.

## Usage notes

- **Semantic deduplication:** a new memory that is too similar to an existing one may be
  rejected. Pass `conversation_id` to store incremental notes from one session.
- Unknown `metadata.type` values are silently changed to `observation`. Use the built-in
  types: `decision`, `learning`, `error`, `gotcha`, `bug`, `reference`, `note`, …
- `max_response_chars` on `memory_search` limits the size of large result sets.

## How it relates to the other memories

- **`docs/claude/memory.md` stays the source of truth** for the project state, decisions,
  limitations and plan. It is versioned and reviewable. Do not move it into the MCP store.
- The MCP memory is for things that are *not* worth a commit, but should outlive a
  session: debugging insights, environment gotchas, "tried X, it failed because Y", and
  facts that matter across projects (e.g. host or Podman quirks).
- xerj also has an agent memory (`xerj_memory_store`), but it can't delete. Prefer this
  server for memory, and use xerj for code and doc search (`docs/claude/mcp-xerj.md`).

## Caveats

- `MCP_ALLOW_ANONYMOUS_ACCESS=true` without `MCP_API_KEY`: anyone who can reach port
  8765/8000 on the host can read and write the memories. Don't store secrets.
- The image is pulled as `:latest`. Server upgrades can change tools or behaviour.
