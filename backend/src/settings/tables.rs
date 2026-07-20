//! Scaling and label tables for settings fields, indexed by the raw wire code.

/// `VERT-CHx-VB` index → millivolts per division. Verified over a full 2 mV…10 V sweep.
///
/// Quirk: 10 V/div also stores index 0 (wraps mod 11), so index 0 is ambiguous between
/// 2 mV/div and 10 V/div.
pub const VB_TO_MV: [u32; 11] = [2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000];

/// `HORIZ-TB` / `HORIZ-WIN-TB` index → time per division in nanoseconds.
///
/// A 2-4-8 sequence across the scope's 2 ns…40 s range (32 steps). `HORIZ-WIN-TB` follows
/// the knob over the full range; `HORIZ-TB` (the real acquisition timebase) clamps at
/// index 6 = 200 ns/div — faster settings are zoom/interpolation.
pub const TB_TO_NS: [u64; 32] = [
    2, 4, 8, 20, 40, 80, 200, 400,
    800, 2_000, 4_000, 8_000, 20_000, 40_000,
    80_000, 200_000, 400_000, 800_000, 2_000_000,
    4_000_000, 8_000_000, 20_000_000, 40_000_000,
    80_000_000, 200_000_000, 400_000_000, 800_000_000,
    2_000_000_000, 4_000_000_000, 8_000_000_000,
    20_000_000_000, 40_000_000_000,
];

/// `ACQURIE-STORE-DEPTH` code → label. Codes are gapped: the missing ones are depths that
/// are greyed out in the current acquisition mode.
pub const ACQ_DEPTH_NAMES: &[(u8, &str)] = &[(0, "4K"), (4, "40K"), (6, "512K"), (7, "1M")];

/// `CONTROL-MENUID` → the on-screen menu it identifies. Partial; unmapped ids exist.
///
/// Multi-page submenus use consecutive ids. Some menus (CH1/CH2, Acquire) hold the id
/// constant while open, so the value is a state rather than an edge.
pub const MENU_NAMES: &[(u8, &str)] = &[
    (1, "CH1 (vertical)"),
    (2, "CH2 (vertical)"),
    (3, "Horizontal p1"),
    (4, "Display (Type/Persist/Contrast)"),
    (5, "Trig:Edge"),
    (6, "Trig:Pulse p1"),
    (7, "Trig:Pulse p2"),
    (8, "Trig:Video"),
    (10, "default/none"),
    (11, "Trigger"),
    (15, "Cursor"),
    (16, "Math:FFT p1"),
    (17, "Acquire"),
    (18, "Save/Recall:SETUP"),
    (19, "Save/Recall:REF"),
    (20, "Measure"),
    (21, "Measure:config"),
    (22, "Trig:Slope p1"),
    (23, "Trig:Slope p2"),
    (24, "Trig:Alter"),
    (25, "Default Setup"),
    (26, "Alter-CH1:Edge"),
    (27, "Alter-CH1:Pulse"),
    (28, "Alter-CH1:Video"),
    (29, "Alter-CH1:Overtime"),
    (30, "Alter-CH2:Edge"),
    (31, "Alter-CH2:Pulse"),
    (32, "Alter-CH2:Video"),
    (33, "Alter-CH2:Overtime"),
    (36, "Display (Grid/Format)"),
    (38, "Trig:Overtime p1"),
    (39, "Trig:Overtime p2"),
    (40, "Horizontal p2"),
    (41, "Math"),
    (42, "Utility p1"),
    (43, "Utility p2"),
    (47, "Save/Recall"),
    (48, "Save/Recall:CSV/FileList"),
    (56, "Math:FFT p2"),
    (61, "Logic Analyzer"),
    (62, "LA config (D7-D0 group)"),
    (63, "LA config (D15-D8 group)"),
];

/// Look up a label in one of the `(code, name)` tables above.
pub fn lookup(table: &[(u8, &'static str)], code: u8) -> Option<&'static str> {
    table.iter().find(|(c, _)| *c == code).map(|(_, name)| *name)
}
