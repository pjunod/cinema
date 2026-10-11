# SPDX-License-Identifier: Apache-2.0
"""Original API adapter for a user-installed libCEC 8.1.6 Python binding.

No upstream source or native binary is included. Combined redistribution
requires a separately approved compatible-license/package decision.
"""
import importlib
from errors import BackendError

PINNED_VERSION = "8.1.6"

class LibCecBackend:
    def __init__(self, device, emit, binding=None):
        self.device, self.emit, self.adapter, self.config = device, emit, None, None
        if binding is None:
            try:
                package = importlib.import_module("cec")
                binding = getattr(package, "cec", package)
            except (ImportError, OSError) as exc:
                raise BackendError("missing_dependency") from exc
        self.binding = binding
        self.opened = False

    def open(self):
        cec = self.binding
        try:
            config = cec.libcec_configuration()
            config.strDeviceName = "Cinema"
            config.bActivateSource = 0
            config.deviceTypes.Add(cec.CEC_DEVICE_TYPE_PLAYBACK_DEVICE)
            config.clientVersion = cec.LIBCEC_VERSION_CURRENT
            config.SetLogCallback(lambda *_: 0)
            config.SetKeyPressCallback(self.key_callback)
            self.config = config  # retain callback/config objects for native lifetime
            self.adapter = cec.ICECAdapter.Create(config)
            if not self.adapter:
                raise BackendError("backend_error")
            if self.adapter.VersionToString(config.serverVersion) != PINNED_VERSION:
                raise BackendError("version_mismatch")
            adapters = self.adapter.DetectAdapters()
            matches = [a for a in adapters if a.strComName == self.device]
            if not matches:
                raise BackendError("no_adapter")
            if not self.adapter.Open(matches[0].strComName):
                # Open(false) does not expose errno: do not invent permission/busy.
                raise BackendError("open_failed")
            self.opened = True
        except BackendError:
            self.close()
            raise
        except PermissionError as exc:
            self.close()
            raise BackendError("permission_denied") from exc
        except OSError as exc:
            self.close()
            raise BackendError("adapter_busy" if exc.errno == 16 else "disconnected") from exc
        except (AttributeError, TypeError, RuntimeError) as exc:
            self.close()
            raise BackendError("backend_error") from exc

    def key_callback(self, key, duration):
        # libCEC's typed key callback: duration zero is press, >zero is release.
        if type(key) is int and 0 <= key <= 255 and type(duration) is int and duration >= 0:
            self.emit("press" if duration == 0 else "release", key)
        return 0

    def poll(self):
        try:
            if not self.opened or not self.adapter.PingAdapter():
                raise BackendError("disconnected")
        except (OSError, RuntimeError) as exc:
            raise BackendError("disconnected") from exc

    def close(self):
        adapter, self.adapter = self.adapter, None
        self.opened = False
        if adapter:
            try:
                adapter.Close()
            except (OSError, RuntimeError):
                pass
        self.config = None
