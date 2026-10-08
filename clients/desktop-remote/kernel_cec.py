# SPDX-License-Identifier: Apache-2.0
"""Original Linux CEC UAPI adapter, using public Linux v6.12 ABI definitions.

Only the configured /dev/cecN is opened. No input-device/key injection or
power/volume/transmit operation is exposed. The kernel supplies HDMI facts.
"""
import ctypes as C
import errno
import os
import platform
import re
import select
import sys
import time
from errors import BackendError

class Caps(C.Structure):
    _fields_ = [("driver", C.c_char * 32), ("name", C.c_char * 32),
                ("available_log_addrs", C.c_uint32), ("capabilities", C.c_uint32), ("version", C.c_uint32)]

class Addresses(C.Structure):
    _fields_ = [("log_addr", C.c_uint8 * 4), ("mask", C.c_uint16), ("version", C.c_uint8),
                ("count", C.c_uint8), ("vendor", C.c_uint32), ("flags", C.c_uint32),
                ("name", C.c_char * 15), ("primary", C.c_uint8 * 4), ("types", C.c_uint8 * 4),
                ("all_types", C.c_uint8 * 4), ("features", (C.c_uint8 * 12) * 4)]

class Message(C.Structure):
    _fields_ = [("tx_ts", C.c_uint64), ("rx_ts", C.c_uint64), ("length", C.c_uint32),
                ("timeout", C.c_uint32), ("sequence", C.c_uint32), ("flags", C.c_uint32),
                ("message", C.c_uint8 * 16), ("reply", C.c_uint8), ("rx_status", C.c_uint8),
                ("tx_status", C.c_uint8), ("arb_lost", C.c_uint8), ("nack", C.c_uint8),
                ("low_drive", C.c_uint8), ("error", C.c_uint8)]

# _IOC for the supported Linux ARM64/x86_64 asm-generic ioctl ABI.
def request(number, shape, direction=3):
    return (direction << 30) | (C.sizeof(shape) << 16) | (ord("a") << 8) | number

def ioctl_value(fd, number, value, direction=3):
    import fcntl
    buffer = bytearray(bytes(value))
    fcntl.ioctl(fd, request(number, type(value), direction), buffer, True)
    return type(value).from_buffer_copy(buffer)

class KernelCecBackend:
    def __init__(self, device, emit, clock=time.monotonic_ns):
        self.device, self.emit, self.clock = device, emit, clock
        self.fd, self.mask, self.claimed = None, 0, False

    def open(self):
        if sys.platform != "linux" or platform.machine().lower() not in ("aarch64", "arm64", "x86_64", "amd64"):
            raise BackendError("unsupported_backend")
        if not re.fullmatch(r"/dev/cec[0-9]+", self.device):
            raise BackendError("invalid_configuration")
        try:
            self.fd = os.open(self.device, os.O_RDWR | os.O_CLOEXEC)
            caps = ioctl_value(self.fd, 0, Caps())
            if caps.capabilities & 6 != 6 or caps.available_log_addrs < 1:
                raise BackendError("unsupported_backend")
            physical = ioctl_value(self.fd, 1, C.c_uint16(), 2)
            if physical.value == 0xffff:
                raise BackendError("waiting_for_tv")
            # Exclusive ownership avoids changing another CEC application's config.
            ioctl_value(self.fd, 9, C.c_uint32(0x22), 1)
            old = ioctl_value(self.fd, 3, Addresses(), 2)
            if old.count or old.mask:
                raise BackendError("adapter_busy")
            addresses = Addresses()
            addresses.version, addresses.count, addresses.vendor = 5, 1, 0xffffffff
            addresses.name = b"Cinema"
            addresses.primary[0], addresses.types[0], addresses.all_types[0] = 4, 3, 0x10
            addresses.flags = 0  # explicitly disable RC passthrough to the OS
            configured = ioctl_value(self.fd, 4, addresses)
            self.claimed = True
            self.mask = configured.mask
            if not self.mask:
                raise BackendError("waiting_for_tv")
            os.set_blocking(self.fd, False)
        except BackendError:
            self.close()
            raise
        except OSError as exc:
            self.close()
            state = "permission_denied" if exc.errno in (errno.EACCES, errno.EPERM) else "adapter_busy" if exc.errno == errno.EBUSY else "no_adapter" if exc.errno == errno.ENOENT else "disconnected"
            raise BackendError(state) from exc

    def decode(self, value):
        # Delayed kernel frames must not become fresh keypresses after a UI stall.
        elapsed = self.clock() - value.rx_ts
        if not value.rx_status & 1 or value.length not in (2, 3) or not 0 <= elapsed < 750_000_000:
            return
        destination = value.message[0] & 15
        if destination == 15 or not self.mask & (1 << destination):
            return
        opcode = value.message[1]
        if opcode == 0x44 and value.length == 3:
            self.emit("press", int(value.message[2]))
        elif opcode == 0x45 and value.length == 2:
            self.emit("release", None)

    def poll(self):
        try:
            physical = ioctl_value(self.fd, 1, C.c_uint16(), 2)
            if physical.value == 0xffff:
                raise BackendError("waiting_for_tv")
            for _ in range(64):
                ready, _, _ = select.select([self.fd], [], [], 0)
                if not ready:
                    break
                self.decode(ioctl_value(self.fd, 6, Message()))
        except BlockingIOError:
            pass
        except OSError as exc:
            raise BackendError("disconnected") from exc

    def close(self):
        fd, self.fd = self.fd, None
        if fd is not None:
            if self.claimed:
                try:
                    ioctl_value(fd, 4, Addresses())
                except OSError:
                    pass
            os.close(fd)
        self.claimed, self.mask = False, 0
