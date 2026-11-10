# add (devcontainer local) vscode mcp server
claude mcp add --transport http vscode http://localhost:3000/mcp

# add memory
claude mcp add --transport http memory http://host.docker.internal:8765/mcp

# find claude (vscode devcontainer install)
find /home/vscode/ -iname claude

# install xerj
curl -fsSL https://xerj.org/get | sh -s -- --with-model
# only for test (maybe it is better to run xerj inside the devcontainer)
XERJ_ALLOW_INSECURE_NETWORK_BIND=true xerj --facts-model-dir /home/tpasch/.local/share/xerj/model --insecure --data-dir ~/dev/xerj-data --bind 0.0.0.0
# this listens to: http://localhost:9200/_facts

# XERJ-Query is off until you start the node with the model directory:
# xerj --facts-model-dir /home/tpasch/.local/share/xerj/model --insecure --data-dir ~/xerj-data
# verify:  curl -s localhost:9200/_facts | jq .model.state   
# ("not_loaded" for the first seconds while the model preloads, then "loaded"; a 404 means the node runs without the flag)

# MCP seems different, see
# https://github.com/xerj-org/xerj/blob/main/docs/MODEL_QUICKSTART.md
