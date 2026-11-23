#!/usr/bin/python3
import os
import subprocess
import sys
import time

assert sys.argv[1:] == ["--version"]
assert os.environ["AVAILABILITY_ENV"] == "provided"
mode = os.environ.get("AVAILABILITY_MODE", "normal")
if "AVAILABILITY_CWD_FILE" in os.environ:
    with open(os.environ["AVAILABILITY_CWD_FILE"], "w") as marker:
        marker.write(os.getcwd())
if mode == "hang":
    with open(os.environ["AVAILABILITY_PID"], "w") as marker:
        marker.write(str(os.getpid()))
    time.sleep(30)
elif mode == "empty":
    pass
elif mode == "error":
    print("dummy-credential-not-for-logs", file=sys.stderr)
    sys.exit(1)
elif mode == "nonzero":
    print("opencode v2.3.2")
    sys.exit(1)
elif mode == "orphan":
    helper = subprocess.Popen(
        ["sleep", "30"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    with open(os.environ["AVAILABILITY_HELPER_PID"], "w") as marker:
        marker.write(str(helper.pid))
    print("opencode v2.3.2", flush=True)
    sys.stdout.close()
    sys.exit(0)
elif mode == "malformed":
    print("opencode v2.3.2 dummy-credential-not-for-logs")
elif mode == "oversized":
    print("x" * 10000)
elif mode == "old":
    print("opencode v1.0.0")
else:
    print("opencode v2.3.2")
