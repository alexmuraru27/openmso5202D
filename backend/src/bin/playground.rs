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

use mso5202d::{Device, Key, Result};

fn main() {
    if let Err(e) = run() {
        eprintln!("\n[playground] error: {e}");
        eprintln!("(is the scope plugged in? is the udev rule installed, or are you root?)");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    println!("[playground] connecting to MSO5202D…");
    let scope = Device::connect()?;
    if let Some((bus, address)) = scope.transport().bus_address() {
        println!("[playground] connected — bus {bus} address {address}");
    }

    // --- settings ------------------------------------------------------------
    let settings = scope.read_settings()?;
    println!("\n[settings]");
    println!("  menu        {:?}", settings.menu_name());
    println!("  trig state  {:?}", settings.trig_state());
    println!("  store depth {:?}", settings.store_depth());
    println!("  time/div    {:?} ns", settings.time_per_div_ns());
    println!("  sample int  {:?} ns", settings.sample_interval_ns());
    for ch in [1u8, 2] {
        println!(
            "  CH{ch}         shown={} {:?} mV/div probe={:?} coupling={:?}",
            settings.channel_shown(ch),
            settings.volts_per_div_mv(ch),
            settings.probe(ch),
            settings.coupling(ch),
        );
    }
    println!("  trig level  {:?} mV", settings.trigger_level_mv());

    // --- screen --------------------------------------------------------------
    match scope.screenshot() {
        Ok(shot) => println!(
            "\n[screen] {}x{}, centre pixel {:?}",
            shot.width(),
            shot.height(),
            shot.pixel(shot.width() / 2, shot.height() / 2)
        ),
        Err(e) => println!("\n[screen] grab failed: {e}"),
    }

    // --- files ---------------------------------------------------------------
    match scope.download("/protocol.inf") {
        Ok(bytes) => println!("\n[files] /protocol.inf: {} bytes", bytes.len()),
        Err(e) => println!("\n[files] download failed: {e}"),
    }

    // --- shell (read-only) ---------------------------------------------------
    match scope.shell("uname -n -r") {
        Ok(out) => println!("[shell] uname: {}", out.trim()),
        Err(e) => println!("[shell] failed: {e}"),
    }
    match scope.list_dir("/mnt/udisk") {
        Ok(entries) if entries.is_empty() => println!("[shell] /mnt/udisk is empty"),
        Ok(entries) => {
            println!("[shell] /mnt/udisk — {} entries", entries.len());
            for entry in entries.iter().take(10) {
                println!("          {:>10}  {}", entry.size, entry.name);
            }
        }
        Err(e) => println!("[shell] list failed: {e} (is a card inserted?)"),
    }

    // --- keys ----------------------------------------------------------------
    // Open a menu and read back which menu the scope reports, then return to where we
    // started. Harmless and reversible — it only moves the on-screen menu.
    let before = scope.read_settings()?.menu_id();
    scope.press(Key::MenuAcquire)?;
    std::thread::sleep(Duration::from_millis(400));
    let after = scope.read_settings()?.menu_id();
    println!("\n[keys] menu {before} -> {after} (17 = Acquire)");
    scope.press(Key::MenuSaveRecall)?;

    println!("\n[playground] done.");
    Ok(())
}
