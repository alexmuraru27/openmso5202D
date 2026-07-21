//! Hardware round-trip: a big data-channel read immediately followed by shell listings.
//!
//! This is the sequence the SD-card panel performs (download, then re-list), and the one
//! that produced `bad leader 0x53 (wanted 0x43)` — a leftover `0x53` frame being read where
//! the shell's `0x43` reply belonged. Ignored by default: needs the scope attached.
use mso5202d::{control, Device};

#[test]
#[ignore = "requires the scope"]
fn shell_survives_a_large_data_channel_read() {
    let device = Device::connect_without_reset().expect("connect");
    device.transport().resync();

    for round in 0..3 {
        let entries = device.list_dir(control::CARD_PATH).expect("list");
        let csvs = control::csv::wavedata_files(&entries);
        println!("round {round}: {} CSVs", csvs.len());
        assert!(!csvs.is_empty(), "card should hold CSVs for this test");

        // A multi-megabyte 0x53 transfer, then straight back to the 0x43 shell.
        let biggest = csvs.iter().max_by_key(|f| f.size).unwrap();
        let path = format!("{}/{}", control::CARD_PATH, biggest.name);
        let data = device.download(&path).expect("download");
        println!("  downloaded {} = {} bytes", biggest.name, data.len());

        let after = device.list_dir(control::CARD_PATH).expect("list right after a big read");
        assert!(!control::csv::wavedata_files(&after).is_empty());
        println!("  shell OK right after the transfer");
    }
}
