# openmso5202D

An open driver + desktop GUI for the **Hantek MSO5202D** oscilloscope (USB `049f:505a`),
reverse-engineered from USB captures of the vendor Windows app.

- **`backend/`** — `mso5202d`, a reusable Rust driver crate. The lowest layer is the USB
  transport (`usb::Transport`): connect / reconnect / reset, interface binding, and the
  reader-thread-before-write transaction dance the scope requires. `Scope` is a thin
  high-level facade on top.
- **`frontend/`** — a [Tauri 2](https://tauri.app) desktop app. It depends on the
  `mso5202d` crate directly, so **launching the app launches the backend**: Tauri opens
  the scope over USB at startup and exposes it to the webview through commands.
- **`docs/`**, **`scripts/`**, **`scope_dump/`** — the reverse-engineering record: the
  wire-protocol spec, the Python reference tooling the Rust port is based on, and the raw
  captures.

The repo is one **Cargo workspace** (`backend` + `frontend/src-tauri`) and one **pnpm
workspace** (`frontend`).

## Prerequisites

- **Rust** (stable) with `cargo`
- **Node** + **pnpm**
- **libusb 1.0** (`libusb-1.0-0-dev`)
- **Tauri Linux deps**: `webkit2gtk-4.1`, `libgtk-3-dev`, plus the usual
  `build-essential`/`libssl-dev` (see the [Tauri prerequisites](https://tauri.app/start/prerequisites/))
- **USB access**: install the udev rule so the scope is reachable without root:
  ```sh
  sudo cp 70-mso5202d.rules /etc/udev/rules.d/
  sudo udevadm control --reload-rules && sudo udevadm trigger
  # then replug the scope
  ```
  Otherwise run the app/playground as root.

## Build & run

The project has **two stages**, driven from the repo root:

### 1. Bootstrap (once, and after dependency changes)

```sh
pnpm bootstrap
```

Installs the JS dependencies (`pnpm install`) and pre-fetches the Rust crates
(`cargo fetch`).

### 2. Build

```sh
pnpm build
```

Builds the release desktop bundle via the Tauri CLI (`vite build` → `cargo build` →
platform installer under `frontend/src-tauri/target/release/bundle/`).

### Develop

```sh
pnpm dev          # hot-reloading Tauri dev app (Vite dev server + the Rust backend)
```

## Backend playground ("hack main")

A scratch binary for driving the driver by hand against real hardware while building out
higher layers — fiddle with commands, dump frames, probe the scope:

```sh
pnpm playground
# or:  cargo run -p mso5202d --bin playground
```

Edit `backend/src/bin/playground.rs` freely; nothing there is load-bearing.

Run the backend's tests + lints:

```sh
pnpm backend:check    # cargo test + cargo clippy on the mso5202d crate
```

## Layout

```
openmso5202D/
├── Cargo.toml                     # workspace root
├── package.json                   # pnpm scripts: bootstrap / build / dev / playground
├── pnpm-workspace.yaml
├── backend/                       # `mso5202d` driver crate
│   └── src/
│       ├── protocol/mod.rs        # 'S'-frame build/verify + wire constants
│       ├── usb/transport.rs       # the low-level USB transport (foundation layer)
│       ├── scope.rs               # high-level facade (settings / file read / keys)
│       └── bin/playground.rs      # the "hack main"
├── frontend/                      # Tauri app
│   ├── src/                       # web UI (Vite + TypeScript)
│   └── src-tauri/                 # Rust shell; depends on `mso5202d`
├── docs/                          # reverse-engineered protocol spec
└── scripts/                       # Python reference tooling + captures
```
