//! Tests for the device layer's pure logic: key/knob mapping, `ls` parsing, the shell
//! safety guard, and framebuffer decoding. None of these touch hardware.

use mso5202d::device::files::parse_ls;
use mso5202d::device::screen::{Screenshot, FRAMEBUFFER_BYTES, SCREEN_HEIGHT, SCREEN_WIDTH};
use mso5202d::device::shell::{check_command, marker_for, output_before_marker, wrap_command};
use mso5202d::{Key, Knob, Turn};

// --- keys and knobs ---------------------------------------------------------

#[test]
fn key_ids_match_the_keyprotocol_index() {
    // Spot-check the ids that control logic depends on most.
    assert_eq!(Key::Fn0.id(), 0);
    assert_eq!(Key::MenuSaveRecall.id(), 11);
    assert_eq!(Key::Autoset.id(), 17);
    assert_eq!(Key::Single.id(), 18);
    assert_eq!(Key::RunStop.id(), 19);
    assert_eq!(Key::DefaultSetup.id(), 21);
    assert_eq!(Key::Ch1Menu.id(), 24);
    assert_eq!(Key::Ch2Menu.id(), 30);
    assert_eq!(Key::ForceTrigger.id(), 47);
    assert_eq!(Key::ProbeCheck.id(), 48);
}

#[test]
fn knobs_map_to_their_key_pairs() {
    assert_eq!(Knob::Ch1VoltsPerDiv.key(Turn::Down), Key::Ch1VoltsDown);
    assert_eq!(Knob::Ch1VoltsPerDiv.key(Turn::Up), Key::Ch1VoltsUp);
    assert_eq!(Knob::TriggerLevel.key(Turn::Up), Key::TriggerLevelUp);
}

#[test]
fn timebase_directions_follow_value_not_key_names() {
    // The vendor names are inverted on this firmware: the "SUB" key (40) moves to a
    // FASTER timebase. Turning the knob Down must mean a smaller time/div.
    assert_eq!(Knob::TimePerDiv.key(Turn::Down), Key::TimeBaseFaster);
    assert_eq!(Knob::TimePerDiv.key(Turn::Up), Key::TimeBaseSlower);
    assert_eq!(Key::TimeBaseFaster.id(), 40);
    assert_eq!(Key::TimeBaseSlower.id(), 41);
}

#[test]
fn only_some_knobs_have_a_push_action() {
    assert_eq!(Knob::TriggerLevel.push_key(), Some(Key::TriggerLevelZero));
    assert_eq!(Knob::Ch1Position.push_key(), Some(Key::Ch1PositionZero));
    // The volts/div and timebase knobs do not push.
    assert_eq!(Knob::Ch1VoltsPerDiv.push_key(), None);
    assert_eq!(Knob::TimePerDiv.push_key(), None);
}

#[test]
fn per_channel_knob_lookup_rejects_bad_channels() {
    assert_eq!(Knob::volts_per_div(1), Some(Knob::Ch1VoltsPerDiv));
    assert_eq!(Knob::volts_per_div(2), Some(Knob::Ch2VoltsPerDiv));
    assert_eq!(Knob::volts_per_div(3), None);
    assert_eq!(Knob::position(0), None);
}

// --- directory listing ------------------------------------------------------

const LS_OUTPUT: &str = "\
total 7524
drwxr-xr-x    2 root     root         16384 Jan  1 00:00 .
drwxr-xr-x    3 root     root             0 Jan  1 00:00 ..
-rwxr-xr-x    1 root     root        400064 Jul 11 14:22 WaveData1410.csv
-rwxr-xr-x    1 root     root         40064 Jul 11 14:25 WaveData1411.csv
drwxr-xr-x    2 root     root          4096 Jul 11 14:30 subdir";

#[test]
fn parse_ls_extracts_names_sizes_and_kind() {
    let entries = parse_ls(LS_OUTPUT);
    assert_eq!(entries.len(), 3, "`total`, `.` and `..` must be skipped");

    assert_eq!(entries[0].name, "WaveData1410.csv");
    assert_eq!(entries[0].size, 400_064);
    assert!(!entries[0].is_dir);

    assert_eq!(entries[2].name, "subdir");
    assert!(entries[2].is_dir);
}

#[test]
fn parse_ls_keeps_names_containing_spaces() {
    let line = "-rwxr-xr-x    1 root     root           123 Jul 11 14:22 my file.csv";
    let entries = parse_ls(line);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "my file.csv");
}

#[test]
fn parse_ls_ignores_noise_lines() {
    // The shell channel can interleave stray text; one bad line must not lose the rest.
    let noisy = format!("ls: some warning\n{LS_OUTPUT}\ngarbage");
    assert_eq!(parse_ls(&noisy).len(), 3);
    assert!(parse_ls("").is_empty());
}

#[test]
fn file_entry_builds_absolute_paths() {
    let entries = parse_ls(LS_OUTPUT);
    assert_eq!(
        entries[0].path_in("/mnt/udisk"),
        "/mnt/udisk/WaveData1410.csv"
    );
    // A trailing slash on the directory must not double up.
    assert_eq!(
        entries[0].path_in("/mnt/udisk/"),
        "/mnt/udisk/WaveData1410.csv"
    );
}

// --- shell safety guard -----------------------------------------------------

#[test]
fn safe_read_only_commands_are_allowed() {
    for command in ["ls -la /mnt/udisk", "uname -a", "df -h", "cat /protocol.inf"] {
        assert!(check_command(command).is_ok(), "{command} should be allowed");
    }
}

#[test]
fn destructive_programs_are_blocked() {
    for command in ["rm -rf /", "dd if=/dev/zero of=/dev/mtd0", "reboot", "mkfs.vfat /dev/sda"] {
        assert!(check_command(command).is_err(), "{command} should be blocked");
    }
}

#[test]
fn destructive_programs_are_blocked_behind_a_path_or_separator() {
    // Basename matching: a full path must not sneak past.
    assert!(check_command("/bin/rm file").is_err());
    // And a destructive program in a later segment must still trip.
    assert!(check_command("ls /tmp; rm -rf /").is_err());
    assert!(check_command("ls /tmp | xargs rm").is_err());
}

#[test]
fn redirection_is_confined_to_the_removable_card() {
    // Exporting to the inserted card is the intended path.
    assert!(check_command("cp /sys.inf /mnt/udisk/x.txt").is_ok());
    assert!(check_command("echo hi > /mnt/udisk/note.txt").is_ok());
    // Redirecting onto the scope's own filesystem would overwrite firmware files.
    assert!(check_command("echo hi > /etc/passwd").is_err());
}

// --- shell reply framing ----------------------------------------------------

#[test]
fn commands_are_wrapped_in_a_brace_group_with_a_marker() {
    let marker = marker_for(7);
    let wrapped = wrap_command("ls /mnt/udisk", &marker);
    // The brace group is what makes the firmware's appended redirect capture everything.
    assert!(wrapped.starts_with("{ "), "must be a brace group: {wrapped}");
    assert!(wrapped.ends_with(" }"));
    assert!(wrapped.contains("ls /mnt/udisk"));
    assert!(wrapped.contains(&marker));
}

#[test]
fn markers_are_unique_per_sequence() {
    assert_ne!(marker_for(1), marker_for(2));
}

#[test]
fn output_is_taken_from_before_the_marker() {
    let marker = marker_for(3);
    let reply = format!("file-a\nfile-b\n{marker}\n");
    assert_eq!(output_before_marker(&reply, &marker), Some("file-a\nfile-b\n"));
}

#[test]
fn a_reply_without_our_marker_is_rejected() {
    // This is the "reply lagging one command behind" case — it must not be mistaken
    // for this command's output.
    assert_eq!(output_before_marker("stale output", &marker_for(4)), None);
}

// --- framebuffer decoding ---------------------------------------------------

#[test]
fn rgb565_decodes_to_full_range_rgb8() {
    let mut raw = vec![0u8; FRAMEBUFFER_BYTES];
    // Pixel 0 = 0xFFFF (white), pixel 1 = 0xF800 (pure red), pixel 2 stays black.
    raw[0..2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    raw[2..4].copy_from_slice(&0xF800u16.to_le_bytes());

    let shot = Screenshot::from_rgb565(&raw).expect("full-size buffer should decode");
    assert_eq!(shot.width(), SCREEN_WIDTH);
    assert_eq!(shot.height(), SCREEN_HEIGHT);
    assert_eq!(shot.rgb().len(), SCREEN_WIDTH * SCREEN_HEIGHT * 3);

    assert_eq!(shot.pixel(0, 0), Some((0xF8, 0xFC, 0xF8)));
    assert_eq!(shot.pixel(1, 0), Some((0xF8, 0, 0)));
    assert_eq!(shot.pixel(2, 0), Some((0, 0, 0)));
}

#[test]
fn short_framebuffer_is_rejected() {
    // A truncated grab must fail rather than render a garbled screen.
    assert!(Screenshot::from_rgb565(&vec![0u8; FRAMEBUFFER_BYTES - 2]).is_none());
}

#[test]
fn pixel_lookup_is_bounds_checked() {
    let shot = Screenshot::from_rgb565(&vec![0u8; FRAMEBUFFER_BYTES]).unwrap();
    assert!(shot.pixel(SCREEN_WIDTH - 1, SCREEN_HEIGHT - 1).is_some());
    assert!(shot.pixel(SCREEN_WIDTH, 0).is_none());
    assert!(shot.pixel(0, SCREEN_HEIGHT).is_none());
}
