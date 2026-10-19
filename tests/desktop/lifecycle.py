"""Exercise the installed native controller against the real local supervisor."""
import json
import os
import time
from common import run, wait_for


class Lifecycle:
    def __init__(self, control, receipt):
        self.control = str(control)
        self.receipt = receipt

    def call(self, *args):
        result = run([self.control, *args], timeout=120)
        value = json.loads(result.stdout)
        if not value.get("ok"):
            raise RuntimeError(f"controller refused {args}: {value.get('error')}")
        return value["result"]

    def state(self, state):
        return wait_for(lambda: (v if (v := self.call("status"))["service_state"] == state else None),
                        seconds=90, description=f"server {state}")

    def four_states(self):
        identity = self.call("status")["installation_id"]
        for startup in ("off", "on"):
            for running in (False, True):
                self.call("stop", "--confirm-interruption")
                self.state("stopped")
                self.call("startup", "set", "server", startup)
                if os.uname().sysname == "Linux":
                    self.call("startup", "set", "tray", "off")
                if running:
                    self.call("start")
                    self.state("running")
                before = self.call("status")
                if before["installation_id"] != identity:
                    raise RuntimeError("installation identity changed during lifecycle")
                wanted = "enabled" if startup == "on" else "disabled"
                if before["server_startup"]["state"] != wanted:
                    raise RuntimeError("startup state differs from requested policy")
                # Toggling tray login must not change the server's running/startup state.
                if os.uname().sysname == "Linux":
                    self.call("startup", "set", "tray", "on")
                    self.call("startup", "set", "tray", "off")
                after = self.call("status")
                for field in ("installation_id", "service_state", "server_startup"):
                    if after[field] != before[field]:
                        raise RuntimeError(f"tray login toggle changed server {field}")
                self.receipt.check(f"four-state-{startup}-{'running' if running else 'stopped'}", after)
        self.call("restart", "--confirm-interruption")
        self.state("running")
        self.receipt.check("restart-and-logs", self.call("logs", "--lines", "20"))

    def stop(self):
        self.call("stop", "--confirm-interruption")
        return self.state("stopped")
