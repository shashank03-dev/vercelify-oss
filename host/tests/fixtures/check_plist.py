"""Exit 0 when the launchd job file parses and runs `vercelify-mesh … up`."""
import plistlib
import sys

with open(sys.argv[1], "rb") as f:
    job = plistlib.load(f)
assert job["Label"] == "dev.vercelify.mesh", job["Label"]
assert job["ProgramArguments"][5] == "up", job["ProgramArguments"]
assert job["KeepAlive"] is True and job["RunAtLoad"] is True
