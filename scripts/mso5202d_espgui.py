#!/usr/bin/env python3
"""GUI control panel for the esp_combo_gen test-signal generator.

Exposes the **whole** ESP serial API — every command the firmware accepts — as a Tk
panel, so a capture session can be driven without retyping CLI invocations:

    protocol · frequency · transmit mode · pattern · burst · gap · trigger · reboot

plus a raw console for anything not on the panel (`help`, `id`, `range`, …).

Run it from `scripts/` (it imports the driver next to it):

    cd scripts && python3 mso5202d_espgui.py [--port /dev/ttyUSB0]

The serial channel and its quirks (non-disturbing open, JSON line replies, the
per-protocol frequency ladders) all come from `mso5202d_espgen.EspGen` — this module is
only the panel, so the two stay in step by construction.

**All serial I/O runs on a worker thread.** The firmware answers `trigger` only once the
whole burst has physically gone out, which at 300 baud is minutes; doing that on the Tk
thread would freeze the window. Commands are queued to the worker and replies come back
through a queue the GUI drains on a timer.

Needs `python3-tk`.
"""
from __future__ import annotations

import argparse
import glob
import json
import queue
import threading
import tkinter as tk
from tkinter import ttk

from mso5202d_espgen import (
    MODES,
    PATTERNS,
    PROTO_DESC,
    PROTOS,
    UNITS,
    EspGen,
    build_capabilities,
    fmt_hz,
    running_settings,
)

# Bits per byte on the wire, used only to size the `trigger` reply timeout. UART's
# start+8+stop is the worst case; SPI/I2C are 8-9. Over-estimating just means waiting
# longer before declaring a timeout, which is the safe direction.
BITS_PER_BYTE = 12
# Floor for the trigger timeout, so a fast line still gets a sane grace period.
TRIGGER_MIN_TIMEOUT = 30.0

# How often the status panel refreshes itself while "auto" is ticked.
AUTO_STATUS_MS = 2000


def list_ports() -> list[str]:
    """Serial ports the ESP might be on, newest-looking first."""
    return sorted(glob.glob("/dev/ttyUSB*") + glob.glob("/dev/ttyACM*"))


class EspWorker(threading.Thread):
    """Owns the serial connection and runs every command off the UI thread.

    Commands go in through `submit`; results come back as `(kind, payload)` events the
    GUI drains. Serialising everything on one thread also means a slow `trigger` simply
    queues the status polls behind it instead of racing them onto the same port.
    """

    def __init__(self):
        super().__init__(daemon=True)
        self._cmds: queue.Queue = queue.Queue()
        self.events: queue.Queue = queue.Queue()
        self._gen: EspGen | None = None

    # --- called from the GUI thread ---------------------------------------
    def submit(self, kind: str, **kw):
        self._cmds.put((kind, kw))

    def stop(self):
        self._cmds.put(("quit", {}))

    # --- worker side -------------------------------------------------------
    def _emit(self, kind: str, payload=None):
        self.events.put((kind, payload))

    def _log(self, text: str):
        self._emit("log", text)

    def run(self):
        while True:
            kind, kw = self._cmds.get()
            if kind == "quit":
                self._close()
                return
            try:
                self._handle(kind, kw)
            except TimeoutError as e:
                self._emit("error", f"{e} (is esp_combo_gen flashed and the port free?)")
            except Exception as e:  # keep the worker alive whatever the board does
                self._emit("error", str(e))

    def _close(self):
        if self._gen is not None:
            self._gen.close()
            self._gen = None

    def _status(self) -> dict:
        """Read the board state, tolerating a stale first reply.

        A session that died mid-write leaves a partial command in the board's RX buffer;
        the next command is appended to that fragment and rejected as malformed, so the
        very first query after connecting can come back `{"ok":false}`. A bare newline
        terminates whatever is stuck there, so the read is retried once behind one — the
        panel should never open on an error object.
        """
        assert self._gen is not None
        last: dict = {}
        for attempt in range(2):
            if attempt:
                try:
                    self._gen.query("", timeout=1.0)  # close off a partial line
                except TimeoutError:
                    pass
            try:
                last = self._gen.query("status")
            except TimeoutError:
                continue
            if last.get("proto"):
                return last
        raise TimeoutError(f"no valid status from the generator (last reply: {last})")

    def _handle(self, kind: str, kw: dict):
        if kind == "connect":
            self._close()
            port, reset = kw["port"], kw.get("reset", False)
            self._log(f"opening {port}{' (reboot)' if reset else ''}…")
            self._gen = EspGen(port, reset=reset)
            self._emit("connected", {"port": port})
            self._emit("status", self._status())
            return

        if kind == "disconnect":
            self._close()
            self._emit("disconnected", None)
            return

        if self._gen is None:
            self._emit("error", "not connected")
            return

        if kind == "status":
            self._emit("status", self._status())
            return

        if kind == "command":
            # One raw firmware command; the reply is the board's own JSON.
            cmd = kw["cmd"]
            timeout = kw.get("timeout")
            self._log(f"> {cmd}")
            reply = self._gen.query(cmd, timeout=timeout)
            self._emit("reply", (cmd, reply))
            # Every command's reply is a full status object, so refresh the panel from
            # it rather than issuing a second round-trip.
            if reply.get("proto"):
                self._emit("status", reply)
            else:
                self._emit("status", self._gen.query("status"))
            return


class EspGui:
    """The control panel."""

    def __init__(self, root: tk.Tk, worker: EspWorker, port: str | None):
        self.root = root
        self.worker = worker
        self.caps: dict = {}
        self.connected = False
        self._syncing = False  # guard so programmatic widget updates don't send commands
        self._freq_by_label: dict[str, int] = {}  # display label -> Hz, per active protocol

        root.title("esp_combo_gen — signal generator")

        self._build_connection()
        self._build_generator()
        self._build_trigger()
        self._build_status()
        self._build_console()

        # Size the window to what the panel actually needs rather than a guessed geometry,
        # so nothing is clipped on open, and forbid shrinking below that — the controls have
        # a genuine minimum. Extra space from a manual resize goes to the console.
        root.update_idletasks()
        root.minsize(root.winfo_reqwidth(), root.winfo_reqheight())

        self._set_connected(False)
        if port:
            self.port_var.set(port)
        self.root.after(80, self._drain)
        if self.port_var.get():
            self.connect()

    # --- layout ------------------------------------------------------------
    def _group(self, title: str, expand: bool = False) -> ttk.LabelFrame:
        """A titled section. `expand` gives this group any spare vertical space when the
        window is resized (only the console wants it)."""
        frame = ttk.LabelFrame(self.root, text=title, padding=8)
        frame.pack(fill="both" if expand else "x", expand=expand,
                   padx=10, pady=(8, 10 if expand else 0))
        return frame

    def _build_connection(self):
        g = self._group("Connection")
        self.port_var = tk.StringVar()
        ports = list_ports()
        self.port_box = ttk.Combobox(g, textvariable=self.port_var, values=ports, width=22)
        self.port_box.grid(row=0, column=0, sticky="w")
        if ports:
            self.port_var.set(ports[0])
        ttk.Button(g, text="Rescan", command=self.rescan).grid(row=0, column=1, padx=4)
        self.connect_btn = ttk.Button(g, text="Connect", command=self.connect)
        self.connect_btn.grid(row=0, column=2, padx=4)
        # A reboot returns the board to its power-on defaults — the only way back to a
        # known state if a session left it in an odd mode.
        ttk.Button(g, text="Reboot ESP", command=self.reboot).grid(row=0, column=3, padx=4)
        self.conn_lbl = ttk.Label(g, text="disconnected", foreground="#a33")
        self.conn_lbl.grid(row=0, column=4, padx=10, sticky="w")

    def _build_generator(self):
        g = self._group("Generator")
        self.gen_frame = g

        ttk.Label(g, text="Protocol").grid(row=0, column=0, sticky="w")
        self.proto_var = tk.StringVar(value=PROTOS[0])
        row = ttk.Frame(g)
        row.grid(row=0, column=1, columnspan=3, sticky="w")
        for p in PROTOS:
            ttk.Radiobutton(row, text=p.upper(), value=p, variable=self.proto_var,
                            command=self.on_proto).pack(side="left", padx=(0, 10))
        self.pins_lbl = ttk.Label(g, text=PROTO_DESC[PROTOS[0]], foreground="#666")
        self.pins_lbl.grid(row=1, column=0, columnspan=4, sticky="w", pady=(0, 6))

        # Frequency is a discrete per-protocol ladder in the firmware; offering the table
        # rather than a free entry means the value shown is the value that will apply.
        ttk.Label(g, text="Frequency").grid(row=2, column=0, sticky="w")
        self.freq_var = tk.StringVar()
        self.freq_box = ttk.Combobox(g, textvariable=self.freq_var, width=22, state="readonly")
        self.freq_box.grid(row=2, column=1, sticky="w")
        self.freq_box.bind("<<ComboboxSelected>>", lambda *_: self.on_freq())
        self.achieved_lbl = ttk.Label(g, text="", foreground="#666")
        self.achieved_lbl.grid(row=2, column=2, columnspan=2, sticky="w", padx=8)

        ttk.Label(g, text="Mode").grid(row=3, column=0, sticky="w", pady=(6, 0))
        self.mode_var = tk.StringVar(value="single")
        row = ttk.Frame(g)
        row.grid(row=3, column=1, columnspan=3, sticky="w", pady=(6, 0))
        for m in MODES:
            ttk.Radiobutton(row, text=m, value=m, variable=self.mode_var,
                            command=self.on_mode).pack(side="left", padx=(0, 10))

        ttk.Label(g, text="Pattern").grid(row=4, column=0, sticky="w")
        self.pattern_var = tk.StringVar(value="ramp")
        row = ttk.Frame(g)
        row.grid(row=4, column=1, columnspan=3, sticky="w")
        for p in PATTERNS:
            ttk.Radiobutton(row, text=p, value=p, variable=self.pattern_var,
                            command=self.on_pattern).pack(side="left", padx=(0, 10))

        ttk.Label(g, text="Burst").grid(row=5, column=0, sticky="w", pady=(6, 0))
        self.burst_var = tk.StringVar(value="1")
        burst = ttk.Spinbox(g, from_=1, to=256, textvariable=self.burst_var, width=8)
        burst.grid(row=5, column=1, sticky="w", pady=(6, 0))
        burst.bind("<Return>", lambda *_: self.on_burst())
        ttk.Button(g, text="Set", command=self.on_burst, width=5).grid(row=5, column=2, sticky="w")

        ttk.Label(g, text="Gap (µs)").grid(row=6, column=0, sticky="w")
        self.gap_var = tk.StringVar(value="auto")
        gap = ttk.Entry(g, textvariable=self.gap_var, width=10)
        gap.grid(row=6, column=1, sticky="w")
        gap.bind("<Return>", lambda *_: self.on_gap())
        ttk.Button(g, text="Set", command=self.on_gap, width=5).grid(row=6, column=2, sticky="w")
        ttk.Label(g, text="0 = continuous, or 'auto'", foreground="#666").grid(
            row=6, column=3, sticky="w", padx=8)

    def _build_trigger(self):
        g = self._group("Trigger — send a burst once, then fall silent")
        ttk.Label(g, text="Bytes").grid(row=0, column=0, sticky="w")
        self.trig_n = tk.StringVar(value="16")
        ttk.Spinbox(g, from_=1, to=8192, textvariable=self.trig_n, width=8).grid(row=0, column=1, sticky="w")
        ttk.Label(g, text="Start index").grid(row=0, column=2, sticky="w", padx=(12, 4))
        self.trig_start = tk.StringVar(value="0")
        ttk.Spinbox(g, from_=0, to=100000, textvariable=self.trig_start, width=8).grid(row=0, column=3, sticky="w")
        self.trig_btn = ttk.Button(g, text="Send burst", command=self.on_trigger)
        self.trig_btn.grid(row=0, column=4, padx=10)
        ttk.Label(g, text="Arm the scope first — this switches the generator to triggered mode.",
                  foreground="#666").grid(row=1, column=0, columnspan=5, sticky="w", pady=(4, 0))

    def _build_status(self):
        g = self._group("Status")
        self.status_lbl = ttk.Label(g, text="—", font=("TkFixedFont", 10), justify="left")
        self.status_lbl.grid(row=0, column=0, sticky="w")
        side = ttk.Frame(g)
        side.grid(row=0, column=1, sticky="ne", padx=8)
        ttk.Button(side, text="Refresh", command=lambda: self.worker.submit("status")).pack()
        self.auto_var = tk.BooleanVar(value=True)
        ttk.Checkbutton(side, text="auto", variable=self.auto_var).pack(pady=(4, 0))
        g.columnconfigure(0, weight=1)

    def _build_console(self):
        g = self._group("Console — any firmware command (help, id, range, proto, freq, …)",
                        expand=True)
        self.cmd_var = tk.StringVar()
        entry = ttk.Entry(g, textvariable=self.cmd_var)
        entry.grid(row=0, column=0, sticky="ew")
        entry.bind("<Return>", lambda *_: self.on_console())
        ttk.Button(g, text="Send", command=self.on_console).grid(row=0, column=1, padx=4)
        g.columnconfigure(0, weight=1)
        g.rowconfigure(1, weight=1)  # the log soaks up any spare height
        self.log = tk.Text(g, height=8, wrap="none", font=("TkFixedFont", 9))
        self.log.grid(row=1, column=0, columnspan=2, sticky="nsew", pady=(6, 0))
        scroll = ttk.Scrollbar(g, command=self.log.yview)
        scroll.grid(row=1, column=2, sticky="ns", pady=(6, 0))
        self.log.configure(yscrollcommand=scroll.set)

    # --- actions -----------------------------------------------------------
    def rescan(self):
        ports = list_ports()
        self.port_box.configure(values=ports)
        if ports and not self.port_var.get():
            self.port_var.set(ports[0])
        self._append(f"ports: {', '.join(ports) or 'none found'}")

    def connect(self):
        if self.connected:
            self.worker.submit("disconnect")
            return
        port = self.port_var.get().strip()
        if not port:
            self._append("no port selected")
            return
        self.worker.submit("connect", port=port)

    def reboot(self):
        port = self.port_var.get().strip()
        if port:
            self.worker.submit("connect", port=port, reset=True)

    def _send(self, cmd: str, timeout: float | None = None):
        if not self.connected:
            self._append("not connected")
            return
        self.worker.submit("command", cmd=cmd, timeout=timeout)

    def on_proto(self):
        if self._syncing:
            return
        # `proto` restores that protocol's own last frequency, which the status reply
        # then feeds back into the frequency list.
        self._send(f"proto {self.proto_var.get()}")

    def on_freq(self):
        if self._syncing:
            return
        hz = self._selected_hz()
        if hz is not None:
            self._send(f"freq {hz}")

    def on_mode(self):
        if not self._syncing:
            self._send(f"mode {self.mode_var.get()}")

    def on_pattern(self):
        if not self._syncing:
            self._send(f"pattern {self.pattern_var.get()}")

    def on_burst(self):
        self._send(f"burst {self.burst_var.get().strip()}")

    def on_gap(self):
        self._send(f"gap {self.gap_var.get().strip()}")

    def on_trigger(self):
        try:
            n = int(self.trig_n.get())
            start = int(self.trig_start.get())
        except ValueError:
            self._append("trigger needs whole numbers")
            return
        # The board replies only when the burst has physically finished, so size the
        # timeout from the wire rate — 8192 bytes at 300 baud really is minutes.
        hz = self._selected_hz() or 115200
        timeout = max(TRIGGER_MIN_TIMEOUT, n * BITS_PER_BYTE / max(hz, 1) + 5.0)
        self._append(f"triggering {n} byte(s) from index {start} (≤{timeout:.0f}s)…")
        self._send(f"trigger {n} {start}", timeout=timeout)

    def on_console(self):
        cmd = self.cmd_var.get().strip()
        if cmd:
            self._send(cmd, timeout=60.0)
            self.cmd_var.set("")

    # --- state -------------------------------------------------------------
    def _selected_hz(self) -> int | None:
        """The Hz value behind the selected frequency label."""
        return self._freq_by_label.get(self.freq_var.get())

    def _set_connected(self, on: bool, port: str = ""):
        self.connected = on
        self.connect_btn.configure(text="Disconnect" if on else "Connect")
        self.conn_lbl.configure(text=f"connected — {port}" if on else "disconnected",
                                foreground="#2a7" if on else "#a33")

    def _apply_status(self, st: dict):
        """Reflect a firmware status reply into the widgets, without re-sending it."""
        self.caps = build_capabilities(st)
        s = running_settings(st)
        self._syncing = True
        try:
            proto = s.get("proto") or PROTOS[0]
            self.proto_var.set(proto)
            self.pins_lbl.configure(text=PROTO_DESC.get(proto, ""))

            # Repopulate the frequency list from the active protocol's ladder.
            table = (self.caps.get("protocols", {}).get(proto, {}) or {}).get("table") or []
            self._freq_by_label = {fmt_hz(hz): int(hz) for hz in table}
            self.freq_box.configure(values=list(self._freq_by_label))
            cur = s.get("freq")
            if cur is not None:
                self.freq_var.set(fmt_hz(cur))

            achieved = s.get("freq_achieved")
            unit = UNITS.get(proto, "freq")
            if achieved not in (None, cur):
                self.achieved_lbl.configure(
                    text=f"{unit} — applied {fmt_hz(achieved)} (rate-limited)")
            else:
                self.achieved_lbl.configure(text=unit)

            if s.get("mode"):
                self.mode_var.set("triggered" if s.get("triggered") else s["mode"])
            if s.get("pattern"):
                self.pattern_var.set(s["pattern"])
            if s.get("burst") is not None:
                self.burst_var.set(str(s["burst"]))
            if s.get("gap_us") is not None:
                self.gap_var.set(str(s["gap_us"]))
        finally:
            self._syncing = False

        gap = s.get("gap_us")
        gap_txt = "0 (continuous)" if gap == 0 else f"{gap} µs"
        self.status_lbl.configure(text="\n".join([
            f"protocol   {s.get('proto')}   ({PROTO_DESC.get(s.get('proto', ''), '')})",
            f"frequency  {fmt_hz(s.get('freq') or 0)}"
            + (f"   applied {fmt_hz(s['freq_achieved'])}"
               if s.get("freq_achieved") not in (None, s.get("freq")) else ""),
            f"mode       {s.get('mode')}   triggered={s.get('triggered')}",
            f"pattern    {s.get('pattern')}   burst {s.get('burst')} B/txn   gap {gap_txt}",
            f"next index {st.get('next_index')}   LA {st.get('la', {}).get('channels', '?')} ch"
            f" ({st.get('la', {}).get('fmt', '')})",
        ]))

    def _append(self, text: str):
        self.log.insert("end", text.rstrip() + "\n")
        self.log.see("end")

    # --- event pump --------------------------------------------------------
    def _drain(self):
        try:
            while True:
                kind, payload = self.worker.events.get_nowait()
                if kind == "connected":
                    self._set_connected(True, payload["port"])
                    self._append(f"connected to {payload['port']}")
                elif kind == "disconnected":
                    self._set_connected(False)
                    self._append("disconnected")
                elif kind == "status":
                    self._apply_status(payload)
                elif kind == "reply":
                    cmd, reply = payload
                    if not reply.get("ok", True):
                        self._append(f"  {cmd}: error — {reply.get('error', reply)}")
                    elif "help" in reply:
                        self._append(json.dumps(reply, indent=2))
                    else:
                        self._append(f"  {cmd}: ok  {json.dumps(running_settings(reply))}")
                elif kind == "log":
                    self._append(payload)
                elif kind == "error":
                    self._append(f"! {payload}")
                    if "not connected" in str(payload):
                        self._set_connected(False)
        except queue.Empty:
            pass
        self.root.after(80, self._drain)

    def poll_status(self):
        if self.connected and self.auto_var.get():
            self.worker.submit("status")
        self.root.after(AUTO_STATUS_MS, self.poll_status)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--port", help="serial port (default: auto-detect ttyUSB*/ttyACM*)")
    args = ap.parse_args()

    worker = EspWorker()
    worker.start()

    root = tk.Tk()
    gui = EspGui(root, worker, args.port or (list_ports() or [None])[0])
    root.after(AUTO_STATUS_MS, gui.poll_status)
    try:
        root.mainloop()
    finally:
        worker.stop()


if __name__ == "__main__":
    main()
