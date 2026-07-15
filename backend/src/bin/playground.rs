//! The "hack main" — a scratch binary for driving the driver against real hardware while
//! building out higher layers. Nothing here is load-bearing; edit it freely to try
//! commands, dump frames, and probe the scope.
//!
//! Run it (needs the scope plugged in + udev rule or root):
//!
//! ```sh
//! cargo run -p mso5202d --bin playground
//! ```

use std::time::Duration;

use mso5202d::{Result, Scope};

fn main() {
    if let Err(e) = run() {
        eprintln!("\n[playground] error: {e}");
        eprintln!("(is the scope plugged in? is the udev rule installed, or are you root?)");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    println!("[playground] connecting to MSO5202D…");
    let scope = Scope::connect()?;

    if let Some((bus, addr)) = scope.transport().bus_address() {
        println!("[playground] connected — bus {bus} address {addr}");
    }

    // 1) Poll the settings block and show its size + a hex preview.
    let settings = scope.read_settings()?;
    println!("[playground] settings block: {} bytes", settings.len());
    println!("[playground]   {}", hex_preview(&settings, 32));

    // 2) A raw transaction: framebuffer/settings/etc. can be poked here.
    //    Example — repeat the settings poll via the transport directly:
    let raw = scope
        .transport()
        .transact_with(&[0x01], Duration::from_millis(3000), 1)?;
    println!("[playground] raw 0x01 reply: {} bytes (echo 0x{:02x})", raw.len(), raw.first().copied().unwrap_or(0));

    // 3) Read a small file off the scope's embedded Linux (safe, read-only).
    match scope.read_file("/protocol.inf") {
        Ok(bytes) => println!("[playground] /protocol.inf: {} bytes", bytes.len()),
        Err(e) => println!("[playground] /protocol.inf read failed: {e}"),
    }

    println!("[playground] done.");
    Ok(())
}

/// First `n` bytes of `data` as spaced hex, with an ellipsis if truncated.
fn hex_preview(data: &[u8], n: usize) -> String {
    let shown = &data[..data.len().min(n)];
    let mut s = shown.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    if data.len() > n {
        s.push_str(" …");
    }
    s
}
