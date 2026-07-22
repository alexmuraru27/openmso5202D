//! End-to-end check that Prepare applies the trigger the UI sent — **needs the scope**.
//!
//! The gap this closes is the seam between the webview and the driver. Every other test
//! builds a `TriggerSetup` in Rust, which cannot catch a field the frontend spells
//! differently or one that quietly defaults because it is `skip_deserializing`. So this
//! starts from the **verbatim JSON the UI hands to the `prepare` command**, captured from
//! the running app, and follows it all the way to what the instrument reports.
//!
//! ```sh
//! cargo test -p openmso5202d --release -- --ignored --nocapture
//! ```

use mso5202d::control::trigger::{self, Polarity, Qualifier, TriggerCoupling, TriggerType};
use mso5202d::control::{self, capture::CaptureSpec, Context, SilentProgress};
use mso5202d::{Device, Settings};
use openmso5202d_lib::api::TriggerConfig;

/// Exactly what the webview sends, captured from the running UI, with the level set to the
/// 11 units that Prepare's 1 V/division makes 440 mV.
const PAYLOAD: &str = r#"{
  "kind": "pulse",
  "source": "ch1",
  "mode": "normal",
  "coupling": "ac",
  "polarity": "negative",
  "videoStandard": "ntsc",
  "videoSync": "alllines",
  "qualifier": "less",
  "level": 11,
  "levelZero": 0,
  "levelApplies": true,
  "alterCh1": {
    "kind": "edge", "polarity": "positive", "coupling": "dc",
    "qualifier": "greater", "videoStandard": "ntsc", "videoSync": "alllines"
  },
  "alterCh2": {
    "kind": "edge", "polarity": "positive", "coupling": "dc",
    "qualifier": "greater", "videoStandard": "ntsc", "videoSync": "alllines"
  },
  "valueTargets": { "pulseWidth": 1200000 },
  "values": []
}"#;

/// The level the UI showed, and so the level the scope must end up at.
const WANTED_MILLIVOLTS: f64 = 440.0;

/// The pulse width the panel was left showing, in picoseconds — 1.2 µs.
const WANTED_PULSE_WIDTH_PS: i64 = 1_200_000;

#[test]
#[ignore = "needs the scope attached"]
fn prepare_applies_the_trigger_the_ui_sent() {
    let config: TriggerConfig =
        serde_json::from_str(PAYLOAD).expect("the UI's payload deserialises");
    let setup = config.to_setup().expect("it translates to a driver setup");

    let scope = Device::connect_without_reset().expect("scope connected");
    scope.transport().resync();

    let spec = CaptureSpec {
        channels: vec![1, 2],
        trigger: Some(setup),
        trigger_position: config.level,
        trigger_values: config
            .value_targets
            .iter()
            .filter_map(|(id, &target)| Some((openmso5202d_lib::api::adjustable_from_id(id)?, target)))
            .collect(),
        ..Default::default()
    };
    let sink = SilentProgress;
    let context = Context::new(&scope, &sink);
    control::execute(&context, &spec.prepare_plan()).expect("prepare runs");

    let settings: Settings = scope.read_settings().expect("settings read back");
    let got = trigger::read(&settings).expect("the trigger reads back");

    assert_eq!(got.kind, TriggerType::Pulse, "trigger type");
    assert_eq!(got.source, trigger::TriggerSource::Ch1, "source");
    assert_eq!(got.mode, trigger::TriggerMode::Normal, "mode");
    assert_eq!(got.coupling, TriggerCoupling::Ac, "coupling");
    assert_eq!(got.polarity, Polarity::Negative, "polarity");
    assert_eq!(got.qualifier, Qualifier::Less, "when");

    // The level is the point of the exercise: what the UI showed as 440 mV has to be what
    // the instrument is triggering at, which it can only be if Prepare set the channel scale
    // the UI assumed *and* the level itself.
    assert_eq!(
        settings.trigger_position(),
        config.level,
        "trigger level, in the scope's own units"
    );
    let millivolts = settings.trigger_level_mv().expect("the scope reports a level");
    assert!(
        (millivolts - WANTED_MILLIVOLTS).abs() < 1.0,
        "scope is triggering at {millivolts} mV, the UI showed {WANTED_MILLIVOLTS} mV"
    );

    // And the knob-only value: the panel edits a target locally, and Prepare is what walks
    // the instrument's knob to it.
    assert_eq!(
        settings.field_signed("TRIG-PULSE-TIME"),
        Some(WANTED_PULSE_WIDTH_PS),
        "pulse width the panel was showing"
    );
}
