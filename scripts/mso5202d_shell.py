#!/usr/bin/env python3
"""
Interactive root shell on the MSO5202D's embedded Linux, over USB.

The scope exposes a second command leader byte, `0x43` ('C'): `43 | 0x11 | <cmd>`
runs an arbitrary shell command on the scope (as root) and returns its stdout.
This wraps it in an SSH-like REPL — type commands, see output. (Channel found via
the sibling project github.com/onnokort/dsoc; see docs/MSO5202D-protocol.md F.4.)

    cd scripts && python3 mso5202d_shell.py
    scope:/ $ uname -a
    Linux Hantek 3.2.35 ...
    scope:/ $ cp /sys.inf /mnt/udisk/x.txt      # export a file to an inserted USB card
    scope:/ $ exit

⚠ SAFETY — the scope runs a WATCHDOG that reboots it if a command stalls the
acquisition app or desyncs USB, and the shell is unfiltered ROOT. This tool:
  * BLOCKS obviously destructive commands (rm/mv/dd/mkfs/reboot/… — see below),
  * still lets you brick things with a clever enough command, so stay READ-ONLY.
Reading files is safest done by `cp <file> /mnt/udisk/` and reading the card on a
PC (large stdout over USB can desync the link). Do NOT read the live acquisition
app's /proc/<pid>/* — that is what tripped the watchdog during development.
"""
import struct
import sys
import threading
import time

import usb.core
from mso5202d import Scope, EP_OUT, EP_IN, TRANSACT_POST_S

LEADER_CMD = 0x43      # 'C' command channel (vs 0x53 'S' data channel)
OP_SHELL = 0x11        # + ASCII command → run on embedded Linux, return stdout
ACK = 0x91             # OP_SHELL | 0x80, the reply's first payload byte

# Commands refused outright — they write/erase/kill and can brick the scope or
# corrupt its flash. Matched against every whitespace/`;`/`|`/`&`-separated token
# (basename), so `/bin/rm` and `rm` both trip. Reading is fine; writing to the
# inserted USB card via `cp`/`mkdir` under /mnt/udisk is allowed.
DESTRUCTIVE = {
    'rm', 'rmdir', 'mv', 'dd', 'mkfs', 'mke2fs', 'mkdosfs', 'fdisk', 'sfdisk',
    'mkfs.vfat', 'format', 'kill', 'killall', 'pkill', 'reboot', 'halt',
    'poweroff', 'shutdown', 'init', 'ubiformat', 'ubidetach', 'flash_erase',
    'flash_eraseall', 'nandwrite', 'nanddump', 'mtd_debug', 'chmod', 'chown',
    'chgrp', 'ln', 'truncate', 'tee', 'mknod', 'insmod', 'rmmod', 'modprobe',
    'mount', 'umount', 'passwd',
}   # note: `sync` is allowed (non-destructive flush); `cp`/`mkdir`/`touch` allowed
   # (writing to the inserted card under /mnt/udisk is the intended export path)


def build43(payload: bytes) -> bytes:
    hdr = bytes([LEADER_CMD]) + struct.pack('<H', len(payload) + 1) + payload
    return hdr + bytes([sum(hdr) & 0xFF])


def unsafe_reason(cmd: str):
    """Return a string reason if `cmd` looks destructive, else None."""
    # split on shell separators; look at each command's first word (the program)
    toks = cmd.replace('|', ' ').replace(';', ' ').replace('&', ' ').split()
    for t in toks:
        name = t.rsplit('/', 1)[-1]
        if name in DESTRUCTIVE:
            return f"'{name}' is blocked (destructive / can brick the scope)"
    # output redirection to anywhere but the USB card overwrites scope files
    for i, ch in enumerate(cmd):
        if ch == '>':
            tail = cmd[i:].lstrip('>').strip()
            if not tail.startswith('/mnt/udisk'):
                return "output redirection (`>`) to a scope path is blocked; " \
                       "redirect only under /mnt/udisk"
    return None


class Shell:
    def __init__(self):
        self.sc = Scope()
        self.cwd = '/'
        self._seq = 0

    @property
    def dev(self):
        return self.sc.dev

    def _flush(self, timeout=200):
        """Drain stale/buffered IN frames so the next reply aligns to our write.
        The scope tends to lag one reply behind after a big output; this fixes it."""
        for _ in range(30):
            try:
                self.dev.read(EP_IN, 512, timeout=timeout)
            except usb.core.USBError:
                return

    def _read_frame(self, timeout):
        rx = bytearray()
        while len(rx) < 3:
            rx.extend(self.dev.read(EP_IN, 512, timeout=timeout))
        total = struct.unpack_from('<H', rx, 1)[0] + 3
        while len(rx) < total:
            rx.extend(self.dev.read(EP_IN, 512, timeout=timeout))
        return bytes(rx[:total])

    def _raw(self, payload: bytes, timeout=4000) -> bytes:
        """One 0x43 transaction: post the bulk-IN read BEFORE the OUT write (the
        transport quirk), then reassemble the reply (0x91 ack + stdout, possibly
        multi-frame)."""
        self._flush()
        out = {}
        posted = threading.Event()

        def reader():
            posted.set()
            try:
                out['f'] = self._read_frame(timeout)
            except Exception as e:
                out['e'] = e

        t = threading.Thread(target=reader, daemon=True)
        t.start()
        posted.wait(0.5)
        time.sleep(TRANSACT_POST_S)
        try:
            self.dev.write(EP_OUT, build43(payload), timeout=2000)
        except usb.core.USBError as e:
            out.setdefault('e', e)
        t.join(timeout / 1000 + 1.5)
        if 'f' not in out:
            raise out.get('e', TimeoutError("no response"))
        text = self._strip(out['f'])
        # gather any follow-up frames (large output arrives multi-frame)
        while True:
            try:
                text += self._strip(self._read_frame(300))
            except usb.core.USBError:
                break
        return text

    @staticmethod
    def _strip(frame: bytes) -> bytes:
        p = frame[3:-1]                      # drop leader|len and checksum
        return p[1:] if p[:1] == bytes([ACK]) else p

    def run(self, cmd: str, timeout=4000) -> str:
        """Run `cmd` in the tracked cwd; return stdout. A unique end-marker guards
        against the scope's /msg reply racing one command behind: re-issue (the
        command is read-only/idempotent) until the reply carries THIS marker."""
        self._seq += 1
        marker = f"__MSOEND{self._seq}__"
        # The firmware appends ` > /msg` to our string, so `a; b` would capture
        # only `b`. Wrap in a brace group so the redirect captures EVERYTHING
        # (cwd change, the command, and the race-guard marker).
        full = f"{{ cd {self._q(self.cwd)} 2>/dev/null; {cmd} ; echo '{marker}' ; }}"
        payload = bytes([OP_SHELL]) + full.encode('latin1')
        last = ''
        for _ in range(5):
            try:
                last = self._raw(payload, timeout=timeout).decode('latin1')
            except Exception as e:
                try:
                    self.sc._resync()          # bad frame/timeout dirties the link
                except Exception:
                    pass
                raise e
            if marker in last:
                return last.split(marker)[0]   # real output precedes the marker
            time.sleep(0.05)                   # stale reply — let /msg settle, retry
        return last

    @staticmethod
    def _q(s: str) -> str:
        return "'" + s.replace("'", "'\\''") + "'"

    def chdir(self, target: str):
        """Update the virtual cwd locally (each command is otherwise stateless, so
        we prepend `cd <cwd>`). Resolved without a scope round-trip to avoid the
        reply race; an invalid path just makes later commands' `cd` fail quietly."""
        import posixpath
        if not target or target == '~':
            target = '/'
        if not target.startswith('/'):
            target = posixpath.join(self.cwd, target)
        self.cwd = posixpath.normpath(target) or '/'
        return None

    def close(self):
        try:
            self.sc.close()
        except Exception:
            pass


BANNER = """\
MSO5202D shell (0x43 channel) — root on the scope's embedded Linux.
  ⚠ READ-ONLY recommended; destructive commands are blocked. A stalling command
    can reboot the scope (watchdog). Export files with `cp X /mnt/udisk/`.
  Type `exit` or Ctrl-D to quit.\
"""


def repl():
    print(BANNER)
    try:
        sh = Shell()
    except Exception as e:
        print(f"cannot open scope: {e}")
        return 1
    try:
        first = sh.run('uname -n -r').strip()
        if first:
            print(f"connected: {first}")
    except Exception as e:
        print(f"[!] initial probe failed: {e}")
    try:
        while True:
            try:
                cmd = input(f"scope:{sh.cwd} $ ").strip()
            except EOFError:
                print()
                break
            if not cmd:
                continue
            if cmd in ('exit', 'quit'):
                break
            reason = unsafe_reason(cmd)
            if reason:
                print(f"[blocked] {reason}")
                continue
            # `cd` updates the tracked cwd; everything else is a stateless command
            if cmd == 'cd' or cmd.startswith('cd '):
                err = sh.chdir(cmd[2:].strip() or '/')
                if err:
                    print(err)
                continue
            try:
                out = sh.run(cmd)
            except KeyboardInterrupt:
                print("\n[interrupted]")
                continue
            except Exception as e:
                print(f"[error] {e}  (link resynced; try again)")
                continue
            sys.stdout.write(out if out.endswith('\n') or not out else out + '\n')
    except KeyboardInterrupt:
        print()
    finally:
        sh.close()
    return 0


if __name__ == '__main__':
    sys.exit(repl())
