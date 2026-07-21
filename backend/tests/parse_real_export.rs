//! Real front-panel Save→CSV exports parse through the same path `load_csvs` uses for a
//! local file, so a saved capture can be reviewed with no instrument attached.

use mso5202d::waveform;

/// A handful of real exports from the decoder corpus.
fn fixtures() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "csv") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(std::path::Path::new("../scope_dump/decoder_corpus"), &mut found);
    found.sort();
    found.truncate(6);
    found
}

#[test]
fn real_exports_parse_from_disk() {
    let files = fixtures();
    assert!(
        !files.is_empty(),
        "expected exported CSVs under scope_dump/decoder_corpus"
    );

    for path in files {
        let text = std::fs::read_to_string(&path).expect("read");
        let csv = waveform::parse_csv(&text).expect("parse");
        let volts = csv.volts.as_ref().expect("an analog export carries volts");
        println!(
            "{}: {} samples, dt={:?}",
            path.file_name().unwrap().to_string_lossy(),
            csv.len(),
            csv.dt_s
        );
        assert!(csv.len() > 100, "a real export has many rows");
        assert_eq!(volts.len(), csv.len(), "one volt reading per timestamp");
        assert!(csv.dt_s.is_some_and(|dt| dt > 0.0), "a real sample interval");
    }
}
