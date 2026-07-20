//! The Save/Recall → CSV menu: reading its on-screen state and naming its files.
//!
//! Two things about this menu are **not** in the settings block, so they can only be read
//! off the rendered screen ([`crate::Device::screenshot`]):
//!
//! - which **Source** radio (CH1/CH2/LA) is selected — the save writes whichever is
//!   highlighted, so getting this wrong silently saves the wrong channel;
//! - whether a save is still **in progress** — the scope shows a banner and ignores key
//!   presses until a large write finalises.
//!
//! The functions here are pure: they take a [`Screenshot`] and return what it shows. The
//! key pressing that acts on those readings lives in the ops.

use crate::device::{FileEntry, Screenshot};

/// Which trace a CSV export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvSource {
    /// Analog channel 1.
    Ch1,
    /// Analog channel 2.
    Ch2,
    /// The 16-channel logic pod.
    La,
}

impl CsvSource {
    /// The three sources in the order the Source softkey cycles them.
    pub const ALL: [CsvSource; 3] = [CsvSource::Ch1, CsvSource::Ch2, CsvSource::La];

    /// Short name, as the menu labels it.
    pub fn name(self) -> &'static str {
        match self {
            CsvSource::Ch1 => "CH1",
            CsvSource::Ch2 => "CH2",
            CsvSource::La => "LA",
        }
    }

    /// Position in the radio group, top to bottom.
    fn row(self) -> usize {
        match self {
            CsvSource::Ch1 => 0,
            CsvSource::Ch2 => 1,
            CsvSource::La => 2,
        }
    }
}

/// Vertical bands of the three Source radio dots, top to bottom.
const SOURCE_ROW_Y: [(usize, usize); 3] = [(58, 72), (80, 94), (102, 116)];

/// Horizontal band the radio dots sit in.
const SOURCE_DOT_X: (usize, usize) = (656, 676);

/// How much further apart the selected dot must be from the unselected pair than they are
/// from each other, before a reading counts as unambiguous.
const SOURCE_MIN_CONTRAST: f64 = 15.0;

/// Screen region the "operation in progress" banner occupies.
const BANNER_Y: (usize, usize) = (230, 245);
const BANNER_X: (usize, usize) = (160, 535);

/// Fraction of banner-coloured pixels above which a save is still running.
const BANNER_THRESHOLD: f64 = 0.04;

/// Which Source radio is selected, or `None` if the reading is ambiguous.
///
/// Rather than matching a fixed accent colour, this picks the **odd one out**: in a radio
/// group the two unselected dots are identical hollow rings and so are the closest-matching
/// pair, leaving the filled one as the selection. That holds whatever colours the firmware
/// theme uses.
///
/// `None` means the menu is not on screen or the grab was poor — never a guess.
pub fn selected_source(screen: &Screenshot) -> Option<CsvSource> {
    let means: Vec<[f64; 3]> = SOURCE_ROW_Y
        .iter()
        .map(|&(y0, y1)| mean_colour(screen, (y0, y1), SOURCE_DOT_X))
        .collect();

    let distance = |a: usize, b: usize| -> f64 {
        means[a]
            .iter()
            .zip(means[b].iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f64>()
            .sqrt()
    };

    // The closest-matching pair are the two hollow dots; the remaining one is selected.
    let pairs = [(0usize, 1usize), (0, 2), (1, 2)];
    let (a, b) = *pairs
        .iter()
        .min_by(|p, q| distance(p.0, p.1).total_cmp(&distance(q.0, q.1)))?;
    let selected = 3 - a - b;

    // Require the selected dot to stand clearly apart from both others, otherwise the group
    // looks uniform and the reading is not trustworthy.
    if distance(selected, a).min(distance(selected, b)) < distance(a, b) + SOURCE_MIN_CONTRAST {
        return None;
    }
    CsvSource::ALL.iter().copied().find(|s| s.row() == selected)
}

/// Whether the scope is still writing a save.
///
/// While the banner is up the scope **ignores key presses**, so acting during this window
/// silently drops whatever was pressed.
pub fn save_in_progress(screen: &Screenshot) -> bool {
    let (y0, y1) = BANNER_Y;
    let (x0, x1) = BANNER_X;
    let mut banner = 0usize;
    let mut total = 0usize;
    for y in y0..y1.min(screen.height()) {
        for x in x0..x1.min(screen.width()) {
            if let Some((r, g, b)) = screen.pixel(x, y) {
                total += 1;
                if r > 160 && b < 100 && (60..190).contains(&g) {
                    banner += 1;
                }
            }
        }
    }
    total > 0 && banner as f64 / total as f64 >= BANNER_THRESHOLD
}

/// Average colour of a screen region.
fn mean_colour(screen: &Screenshot, (y0, y1): (usize, usize), (x0, x1): (usize, usize)) -> [f64; 3] {
    let mut sum = [0f64; 3];
    let mut count = 0f64;
    for y in y0..y1.min(screen.height()) {
        for x in x0..x1.min(screen.width()) {
            if let Some((r, g, b)) = screen.pixel(x, y) {
                sum[0] += r as f64;
                sum[1] += g as f64;
                sum[2] += b as f64;
                count += 1.0;
            }
        }
    }
    if count == 0.0 {
        return [0.0; 3];
    }
    [sum[0] / count, sum[1] / count, sum[2] / count]
}

/// The exported-waveform files among a directory listing, oldest name first.
pub fn wavedata_files(entries: &[FileEntry]) -> Vec<FileEntry> {
    let mut files: Vec<FileEntry> = entries
        .iter()
        .filter(|entry| is_wavedata(&entry.name))
        .cloned()
        .collect();
    files.sort_by_key(|entry| (wavedata_number(&entry.name), entry.name.clone()));
    files
}

/// Whether a filename is one of the scope's exported waveform CSVs.
pub fn is_wavedata(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("wavedata") && lower.ends_with(".csv")
}

/// The sequence number the scope embeds in an exported filename, for ordering.
pub fn wavedata_number(name: &str) -> u64 {
    let digits: String = name.chars().skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}
