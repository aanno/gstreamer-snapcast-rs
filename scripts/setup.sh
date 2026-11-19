###
# NOT FOR REAL USE, only notes/templates for devcontainer setup
###

# install claude
curl -fsSL https://claude.ai/install.sh | bash

# add (devcontainer local) vscode mcp server
# for connection to vs code _outside_ devcontainer, you need a proxy (e.g. supergateway)
claude mcp add --transport http vscode http://host.docker.internal:3001/mcp

# _outside_ devcontainer
pnpx -y supergateway --streamableHttp "http://localhost:3000/mcp" --host 0.0.0.0 --port 3001 --outputTransport streamableHttp
## warning
# No API key is set, so anyone who can reach port 3001 on this machine (the gateway listens on every network interface) 
# can use the MCP server. Set --apiKey or SUPERGATEWAY_API_KEY to require a key, or --host 127.0.0.1
# 
## problem (on vscode extension JuehangQin.vscode-mcp-server)
# [supergateway] StreamableHttp error: Error: Bad Request: Unsupported protocol version: 2026-07-28 (supported versions: 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05, 2024-10-07)

# add memory - also works for connection to doobidoo-mcp-memory-server.yml _outside_ devcontainer
claude mcp add --transport http memory http://host.docker.internal:8765/mcp

# find claude (vscode devcontainer install)
find /home/vscode/ -iname claude

mkdir -p /home/vscode/.local/bin
ln -s /home/vscode/.vscode-server/extensions/anthropic.claude-code-2.1.327-linux-x64/resources/native-binary/claude ~/.local/bin

# xerj _outside_ devcontainer
claude mcp add --transport http xerj "http://host.docker.internal:9201/mcp"

# install xerj
curl -fsSL https://xerj.org/get | sh -s -- --with-model
# only for test (maybe it is better to run xerj inside the devcontainer)
XERJ_DISABLE_FEEDBACK=true XERJ_ALLOW_INSECURE_NETWORK_BIND=true xerj --facts-model-dir /home/tpasch/.local/share/xerj/model --insecure --data-dir ~/dev/xerj-data --bind 0.0.0.0
# this listens to: http://localhost:9200/_facts

# XERJ-Query is off until you start the node with the model directory:
# xerj --facts-model-dir /home/tpasch/.local/share/xerj/model --insecure --data-dir ~/xerj-data
# verify:  curl -s localhost:9200/_facts | jq .model.state   
# ("not_loaded" for the first seconds while the model preloads, then "loaded"; a 404 means the node runs without the flag)

# MCP seems different, see
# https://github.com/xerj-org/xerj/blob/main/docs/MODEL_QUICKSTART.md
#
# _outside_ devcontainer
# start a MCP stdio server: xerj mcp
# start a proxy
pnpx -y supergateway --stdio "xerj mcp" --outputTransport streamableHttp --port 9201 --host 0.0.0.0

# for xerj to actually index the source, you have to autoindex it, e.g.
# _outside_ devcontainer
cd gstreamer-snapcast-r
xerj autoindex .
