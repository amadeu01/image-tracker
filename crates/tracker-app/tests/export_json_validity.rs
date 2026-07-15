//! Confirms tracker-core's hand-rolled `export_json` (task 3.3) produces
//! valid, well-shaped JSON, by parsing it with `serde_json` (a real parser,
//! not just string assertions) — tracker-core stays dependency-free while
//! this crate verifies the contract.

use tracker_core::bar_path::{BarPath, Timebase};
use tracker_core::calibration::Calibration;
use tracker_core::export::export_json;
use tracker_core::geometry::Point;
use tracker_core::session::{Sample, Source};

fn sample_path() -> BarPath {
    let tb = Timebase::new(30, 1).unwrap();
    let samples = vec![
        Sample {
            frame_index: 0,
            position: Point::new(10.0, 20.0),
            source: Source::Tracked,
        },
        Sample {
            frame_index: 1,
            position: Point::new(12.0, 22.0),
            source: Source::Interpolated,
        },
    ];
    BarPath::new(&samples, &[], tb, 0)
}

#[test]
fn json_export_parses_and_has_expected_shape_without_calibration() {
    let path = sample_path();
    let json = export_json(&path, None);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let arr = value.as_array().expect("top-level array");
    assert_eq!(arr.len(), 2);

    let first = &arr[0];
    assert_eq!(first["frame_index"], 0);
    assert_eq!(first["x_px"], 10.0);
    assert_eq!(first["y_px"], 20.0);
    assert!(first["x_m"].is_null());
    assert!(first["y_m"].is_null());
    assert_eq!(first["gap_flag"], false);

    assert_eq!(arr[1]["gap_flag"], true);
}

#[test]
fn json_export_parses_and_has_meter_columns_with_calibration() {
    let path = sample_path();
    let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
    let json = export_json(&path, Some(&cal));
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let arr = value.as_array().expect("top-level array");

    let expected_x_m = cal.px_to_meters(10.0);
    assert!((arr[0]["x_m"].as_f64().unwrap() - expected_x_m).abs() < 1e-6);
}
