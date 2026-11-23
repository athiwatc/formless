#!/usr/bin/env python3
import json
import os
import signal
import sys

if "OPENCODE_START_MARKER" in os.environ:
    with open(os.environ["OPENCODE_START_MARKER"], "w") as marker:
        marker.write("started")


def read():
    return json.loads(sys.stdin.readline())


def send(message):
    print(json.dumps({"jsonrpc": "2.0", **message}), flush=True)


def reply(request, result):
    send({"id": request["id"], "result": result})


assert sys.argv[1:] == ["acp"]
assert os.path.samefile(os.environ["PWD"], os.getcwd())
assert "PROBE_ENV" not in os.environ
config = json.loads(os.environ["OPENCODE_CONFIG_CONTENT"])
assert config["default_agent"] == "build"
assert "mcp" not in config
assert "cap-secret" not in json.dumps(config)
assert "FORMLESS_TOPIC_CAPABILITY" not in json.dumps(config)
if "TOPIC_PROMPT_MARKER" in os.environ:
    for name in ("FORMLESS_TOPIC_CAPABILITY", "FORMLESS_DATA_DIR", "FORMLESS_CONFIG_DIR", "FORMLESS_TOPIC_OUTPUTS_DIR"):
        assert name not in os.environ
request = read()
assert request["method"] == "initialize"
assert request["params"]["clientCapabilities"]["elicitation"] == {"form": {}}
reply(request, {"protocolVersion": 1, "agentInfo": {"name": "OpenCode", "version": "2.3.2"}})
request = read()
assert request["method"] == "session/new"
assert os.path.samefile(request["params"]["cwd"], os.getcwd())
assert request["params"]["mcpServers"][0]["name"] == "formless"
server = request["params"]["mcpServers"][0]
if "MCP_CAPTURE_FILE" in os.environ:
    with open(os.environ["MCP_CAPTURE_FILE"], "w") as capture:
        json.dump(server, capture)
else:
    assert server["env"] == [{"name": "PROBE_ENV", "value": "cap-secret"}]
reply(request, {"sessionId": "fake-session", "configOptions": [{
    "id": "mode", "category": "mode", "type": "select", "currentValue": "user-default",
    "options": [{"value": "build", "name": "Build"}],
}]})
request = read()
assert request["method"] == "session/set_config_option"
assert request["params"]["value"] == "build"
reply(request, {})
prompt = read()
assert prompt["method"] == "session/prompt"
if "TOPIC_PROMPT_MARKER" in os.environ:
    assert os.environ["TOPIC_PROMPT_MARKER"] in json.dumps(prompt["params"])
if "CANCEL_PROBE" in os.environ:
    signal.alarm(10)
    with open(os.environ["CANCEL_PROBE"], "w") as probe:
        probe.write("ready")
    assert read()["method"] == "session/cancel"
    with open(os.environ["CANCEL_PROBE"], "w") as probe:
        probe.write("cancelled")
    reply(prompt, {"stopReason": "cancelled"})
    sys.exit(0)
send({"id": 90, "method": "session/request_permission", "params": {
    "sessionId": "fake-session",
    "toolCall": {"name": "execute", "title": "execute", "kind": "other",
                 "rawInput": {"code": "return await tools.other.echo_env()"}},
    "options": [
        {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
        {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
        {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
    ],
}})
assert read()["result"]["outcome"] == {"outcome": "selected", "optionId": "once"}
with open(os.environ["QUESTION_FIXTURE"]) as fixture:
    question = json.load(fixture)
send({"id": 91, "method": "elicitation/create", "params": question})
assert read()["result"] == {"action": "accept", "content": {"q0": "Blue"}}
send({"method": "session/update", "params": {"sessionId": "fake-session", "update": {
    "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "done"},
}}})
reply(prompt, {"stopReason": "end_turn"})
