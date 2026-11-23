import json
import os
import sys

for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "probe", "version": "0"},
        }
    elif method == "tools/list":
        result = {"tools": [{
            "name": "echo_env",
            "description": "Return the PROBE_ENV marker",
            "inputSchema": {"type": "object", "properties": {}},
        }]}
    elif method == "tools/call":
        result = {"content": [{
            "type": "text",
            "text": json.dumps({"PROBE_ENV": os.environ.get("PROBE_ENV")}),
        }]}
        with open(sys.argv[1], "a") as log:
            log.write(json.dumps({"server": os.environ.get("PROBE_ENV")}) + "\n")
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
