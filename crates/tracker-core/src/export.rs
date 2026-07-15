//! CSV/JSON export of a `BarPath` (task 3.3): `frame_index, t_seconds,
//! x_px, y_px, x_m, y_m, gap_flag`, plus optional velocity columns (task
//! 5.2): `vx, vy, speed, velocity_unit, velocity_interpolated`.
//!
//! Kept in tracker-core, hand-rolled (no `csv`/`serde_json` dependency):
//! the output is a flat table of a handful of numeric columns, trivial to
//! build as strings directly, and keeping tracker-core dependency-free
//! (per CONTEXT.md/PLAN.md's crate split) is worth more than pulling in a
//! serialization crate for this. tracker-app's dev-dependencies may still
//! parse the JSON with `serde_json` in tests to check validity/shape.
//!
//! Coordinate convention: `x_px`/`y_px` are image-pixel coordinates,
//! origin top-left, y increasing downward (screen/image convention, not
//! math convention). `x_m`/`y_m` are the same coordinates scaled by
//! `1.0 / Calibration::px_per_meter()` — i.e. *relative* distances in
//! meters using the same origin and axis directions as the pixel
//! coordinates, not an absolute real-world position. Without a
//! `Calibration`, the meter columns are empty (CSV) / `null` (JSON).
//! `gap_flag` is `true` when the point's `source` is `Source::Interpolated`
//! (see CONTEXT.md's "Gap" term).
//!
//! `x_px`/`y_px` (the raw tracked/interpolated positions) are always taken
//! straight from the `BarPath`, unaffected by whether a velocity series is
//! also exported — smoothing only ever touches the derived `vx`/`vy`/
//! `speed` columns, never the raw position columns, per PLAN.md 5.2 ("raw
//! positions preserved in export"). When no `velocity` is passed, the
//! velocity columns are empty (CSV) / `null` (JSON). `velocity_unit` is
//! `"px/s"` or `"m/s"` per `VelocityUnit`; `velocity_interpolated` mirrors
//! `VelocitySample::from_interpolated` (see velocity.rs's "honest numbers"
//! doc comment) so consumers can filter derived samples that touch a
//! coasted-over Gap.

use crate::bar_path::BarPath;
use crate::calibration::Calibration;
use crate::session::Source;
use crate::velocity::{VelocitySample, VelocityUnit};
use std::collections::HashMap;

fn x_m(x_px: f64, cal: Option<&Calibration>) -> Option<f64> {
    cal.map(|c| c.px_to_meters(x_px))
}

fn unit_str(unit: VelocityUnit) -> &'static str {
    match unit {
        VelocityUnit::PixelsPerSecond => "px/s",
        VelocityUnit::MetersPerSecond => "m/s",
    }
}

fn velocity_by_frame(velocity: Option<&[VelocitySample]>) -> HashMap<u64, VelocitySample> {
    velocity
        .map(|v| v.iter().map(|s| (s.frame_index, *s)).collect())
        .unwrap_or_default()
}

/// Serializes `path` to CSV with header `frame_index,t_seconds,x_px,y_px,
/// x_m,y_m,gap_flag,vx,vy,speed,velocity_unit,velocity_interpolated`. When
/// `cal` is `None`, the `x_m`/`y_m` fields are empty. When `velocity` is
/// `None`, or a given point has no matching `VelocitySample` (by
/// `frame_index`), the velocity fields are empty. Floats are formatted with
/// 6 decimal places for stability across platforms.
pub fn export_csv(
    path: &BarPath,
    cal: Option<&Calibration>,
    velocity: Option<&[VelocitySample]>,
) -> String {
    let vel_by_frame = velocity_by_frame(velocity);
    let mut out = String::from(
        "frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag,vx,vy,speed,velocity_unit,velocity_interpolated\n",
    );
    for p in path.points() {
        let xm = x_m(p.position.x, cal);
        let ym = x_m(p.position.y, cal);
        let xm_field = xm.map(|v| format!("{v:.6}")).unwrap_or_default();
        let ym_field = ym.map(|v| format!("{v:.6}")).unwrap_or_default();
        let gap_flag = p.source == Source::Interpolated;
        let vel = vel_by_frame.get(&p.frame_index);
        let vx_field = vel.map(|v| format!("{:.6}", v.vx)).unwrap_or_default();
        let vy_field = vel.map(|v| format!("{:.6}", v.vy)).unwrap_or_default();
        let speed_field = vel.map(|v| format!("{:.6}", v.speed)).unwrap_or_default();
        let unit_field = vel
            .map(|v| unit_str(v.unit).to_string())
            .unwrap_or_default();
        let interp_field = vel
            .map(|v| v.from_interpolated.to_string())
            .unwrap_or_default();
        out.push_str(&format!(
            "{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{},{}\n",
            p.frame_index,
            p.t_seconds,
            p.position.x,
            p.position.y,
            xm_field,
            ym_field,
            gap_flag,
            vx_field,
            vy_field,
            speed_field,
            unit_field,
            interp_field
        ));
    }
    out
}

/// Serializes `path` to a JSON array of objects, one per point, with keys
/// `frame_index, t_seconds, x_px, y_px, x_m, y_m, gap_flag, vx, vy, speed,
/// velocity_unit, velocity_interpolated`. When `cal` is `None`, `x_m`/`y_m`
/// are JSON `null`. When `velocity` is `None`, or a point has no matching
/// `VelocitySample`, the velocity fields are `null`.
pub fn export_json(
    path: &BarPath,
    cal: Option<&Calibration>,
    velocity: Option<&[VelocitySample]>,
) -> String {
    let vel_by_frame = velocity_by_frame(velocity);
    let mut out = String::from("[\n");
    let points = path.points();
    for (i, p) in points.iter().enumerate() {
        let xm = x_m(p.position.x, cal);
        let ym = x_m(p.position.y, cal);
        let xm_field = xm
            .map(|v| format!("{v:.6}"))
            .unwrap_or_else(|| "null".to_string());
        let ym_field = ym
            .map(|v| format!("{v:.6}"))
            .unwrap_or_else(|| "null".to_string());
        let gap_flag = p.source == Source::Interpolated;
        let vel = vel_by_frame.get(&p.frame_index);
        let vx_field = vel
            .map(|v| format!("{:.6}", v.vx))
            .unwrap_or_else(|| "null".to_string());
        let vy_field = vel
            .map(|v| format!("{:.6}", v.vy))
            .unwrap_or_else(|| "null".to_string());
        let speed_field = vel
            .map(|v| format!("{:.6}", v.speed))
            .unwrap_or_else(|| "null".to_string());
        let unit_field = vel
            .map(|v| format!("\"{}\"", unit_str(v.unit)))
            .unwrap_or_else(|| "null".to_string());
        let interp_field = vel
            .map(|v| v.from_interpolated.to_string())
            .unwrap_or_else(|| "null".to_string());
        out.push_str(&format!(
            "  {{\"frame_index\": {}, \"t_seconds\": {:.6}, \"x_px\": {:.6}, \"y_px\": {:.6}, \"x_m\": {}, \"y_m\": {}, \"gap_flag\": {}, \"vx\": {}, \"vy\": {}, \"speed\": {}, \"velocity_unit\": {}, \"velocity_interpolated\": {}}}",
            p.frame_index, p.t_seconds, p.position.x, p.position.y, xm_field, ym_field, gap_flag,
            vx_field, vy_field, speed_field, unit_field, interp_field
        ));
        if i + 1 < points.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar_path::Timebase;
    use crate::geometry::Point;
    use crate::session::Sample;
    use crate::velocity::velocity_series;

    fn sample(frame_index: u64, x: f64, y: f64, source: Source) -> Sample {
        Sample {
            frame_index,
            position: Point::new(x, y),
            source,
        }
    }

    fn path_with_gap() -> BarPath {
        let tb = Timebase::new(30, 1).unwrap();
        let samples = vec![
            sample(0, 10.0, 20.0, Source::Tracked),
            sample(1, 12.0, 22.0, Source::Interpolated),
        ];
        BarPath::new(&samples, &[], tb, 0)
    }

    #[test]
    fn csv_has_expected_header() {
        let path = path_with_gap();
        let csv = export_csv(&path, None, None);
        let header = csv.lines().next().unwrap();
        assert_eq!(
            header,
            "frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag,vx,vy,speed,velocity_unit,velocity_interpolated"
        );
    }

    #[test]
    fn csv_has_one_row_per_point() {
        let path = path_with_gap();
        let csv = export_csv(&path, None, None);
        // header + 2 rows + trailing newline -> 3 non-empty lines
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn csv_gap_flag_reflects_interpolated_source() {
        let path = path_with_gap();
        let csv = export_csv(&path, None, None);
        let lines: Vec<&str> = csv.lines().collect();
        // frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag,vx,vy,speed,velocity_unit,velocity_interpolated
        let fields1: Vec<&str> = lines[1].split(',').collect();
        let fields2: Vec<&str> = lines[2].split(',').collect();
        assert_eq!(fields1[6], "false");
        assert_eq!(fields2[6], "true");
    }

    #[test]
    fn csv_velocity_columns_empty_without_velocity() {
        let path = path_with_gap();
        let csv = export_csv(&path, None, None);
        let lines: Vec<&str> = csv.lines().collect();
        let fields: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(fields[7], "");
        assert_eq!(fields[8], "");
        assert_eq!(fields[9], "");
        assert_eq!(fields[10], "");
        assert_eq!(fields[11], "");
    }

    #[test]
    fn csv_velocity_columns_populated_when_velocity_given() {
        let path = path_with_gap();
        let velocity = velocity_series(path.points(), 1, None).unwrap();
        let csv = export_csv(&path, None, Some(&velocity));
        let lines: Vec<&str> = csv.lines().collect();
        let fields: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(fields[7], format!("{:.6}", velocity[0].vx));
        assert_eq!(fields[8], format!("{:.6}", velocity[0].vy));
        assert_eq!(fields[9], format!("{:.6}", velocity[0].speed));
        assert_eq!(fields[10], "px/s");
        assert_eq!(fields[11], velocity[0].from_interpolated.to_string());
    }

    #[test]
    fn csv_meter_columns_empty_without_calibration() {
        let path = path_with_gap();
        let csv = export_csv(&path, None, None);
        let lines: Vec<&str> = csv.lines().collect();
        // frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag
        let fields: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(fields[4], "");
        assert_eq!(fields[5], "");
    }

    #[test]
    fn csv_meter_columns_populated_with_calibration() {
        let path = path_with_gap();
        // 200 px = 0.45 m -> px_per_meter ~444.44
        let cal = Calibration::new(Point::new(0.0, 0.0), Point::new(200.0, 0.0), 0.45).unwrap();
        let csv = export_csv(&path, Some(&cal), None);
        let lines: Vec<&str> = csv.lines().collect();
        let fields: Vec<&str> = lines[1].split(',').collect();
        let expected_x_m = cal.px_to_meters(10.0);
        assert_eq!(fields[4], format!("{expected_x_m:.6}"));
        let expected_y_m = cal.px_to_meters(20.0);
        assert_eq!(fields[5], format!("{expected_y_m:.6}"));
    }

    #[test]
    fn json_is_array_with_one_object_per_point() {
        let path = path_with_gap();
        let json = export_json(&path, None, None);
        assert!(json.trim_start().starts_with('['));
        assert!(json.trim_end().ends_with(']'));
        // Two objects -> exactly one comma separating them.
        assert_eq!(json.matches("frame_index").count(), 2);
    }

    #[test]
    fn json_meter_fields_null_without_calibration() {
        let path = path_with_gap();
        let json = export_json(&path, None, None);
        assert!(json.contains("\"x_m\": null"));
        assert!(json.contains("\"y_m\": null"));
    }

    #[test]
    fn json_gap_flag_reflects_interpolated_source() {
        let path = path_with_gap();
        let json = export_json(&path, None, None);
        assert!(json.contains("\"gap_flag\": false"));
        assert!(json.contains("\"gap_flag\": true"));
    }

    #[test]
    fn json_velocity_fields_null_without_velocity() {
        let path = path_with_gap();
        let json = export_json(&path, None, None);
        assert!(json.contains("\"vx\": null"));
        assert!(json.contains("\"vy\": null"));
        assert!(json.contains("\"speed\": null"));
        assert!(json.contains("\"velocity_unit\": null"));
        assert!(json.contains("\"velocity_interpolated\": null"));
    }

    #[test]
    fn json_velocity_fields_populated_when_velocity_given() {
        let path = path_with_gap();
        let velocity = velocity_series(path.points(), 1, None).unwrap();
        let json = export_json(&path, None, Some(&velocity));
        assert!(json.contains("\"velocity_unit\": \"px/s\""));
        assert!(!json.contains("\"vx\": null"));
    }
}
