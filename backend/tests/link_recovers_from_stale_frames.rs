//! A settings read must survive a stale framebuffer left on the link.
//!
//! Reproduces the reported failure: an abandoned `0x20` grab leaves ~75 valid `0x53` frames
//! queued, and the next `0x01` read picks one up — surfacing as
//! `not a settings payload: 10210 bytes`. Those frames pass every transport check (correct
//! leader, length and checksum), so only the settings parse can notice, which is why the
//! retry there has to drain rather than simply try again.
//!
//! Ignored by default: needs the scope attached.
use mso5202d::Device;

#[test]
#[ignore = "requires the scope"]
fn settings_read_recovers_after_an_abandoned_framebuffer() {
    let device = Device::connect_without_reset().expect("connect");
    device.clear_link();

    for round in 0..3 {
        // Ask for a screen, then walk away from it mid-stream, leaving frames queued.
        let _ = device.transport().transact_with(
            &[0x20],
            std::time::Duration::from_millis(4000),
            0,
        );

        // Without the drain-on-misparse this reads a 10210-byte framebuffer frame.
        let settings = device
            .read_settings()
            .unwrap_or_else(|e| panic!("round {round}: settings read did not recover: {e}"));
        println!(
            "round {round}: recovered — menu {}, depth {:?}",
            settings.menu_id(),
            settings.store_depth()
        );
    }

    // And the link is usable afterwards.
    device.clear_link();
    assert!(device.read_settings().is_ok(), "link left unusable");
}
