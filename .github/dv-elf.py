"""Task-only: print the plurxd unit-test executable from cargo JSON output."""
import json
import sys

for line in open(sys.argv[1], encoding="utf-8"):
    if not line.startswith("{"):
        continue
    message = json.loads(line)
    if (message.get("reason") == "compiler-artifact" and message.get("executable")
            and message["target"]["name"] == "plurxd" and message["profile"]["test"]):
        print(message["executable"])
