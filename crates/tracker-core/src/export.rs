//! CSV/JSON export of a `BarPath` (task 3.3): `frame_index, t_seconds,
//! x_px, y_px, x_m, y_m, gap_flag`.
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

use crate::bar_path::BarPath;
use crate::calibration::Calibration;
use crate::session::Source;

fn x_m(x_px: f64, cal: Option<&Calibration>) -> Option<f64> {
    cal.map(|c| c.px_to_meters(x_px))
}

/// Serializes `path` to CSV with header
/// `frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag`. When `cal` is
/// `None`, the `x_m`/`y_m` fields are empty. Floats are formatted with 6
/// decimal places for stability across platforms.
pub fn export_csv(path: &BarPath, cal: Option<&Calibration>) -> String {
    let mut out = String::from("frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag\n");
    for p in path.points() {
        let xm = x_m(p.position.x, cal);
        let ym = x_m(p.position.y, cal);
        let xm_field = xm.map(|v| format!("{v:.6}")).unwrap_or_default();
        let ym_field = ym.map(|v| format!("{v:.6}")).unwrap_or_default();
        let gap_flag = p.source == Source::Interpolated;
        out.push_str(&format!(
            "{},{:.6},{:.6},{:.6},{},{},{}\n",
            p.frame_index, p.t_seconds, p.position.x, p.position.y, xm_field, ym_field, gap_flag
        ));
    }
    out
}

/// Serializes `path` to a JSON array of objects, one per point, with keys
/// `frame_index, t_seconds, x_px, y_px, x_m, y_m, gap_flag`. When `cal` is
/// `None`, `x_m`/`y_m` are JSON `null`.
pub fn export_json(path: &BarPath, cal: Option<&Calibration>) -> String {
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
        out.push_str(&format!(
            "  {{\"frame_index\": {}, \"t_seconds\": {:.6}, \"x_px\": {:.6}, \"y_px\": {:.6}, \"x_m\": {}, \"y_m\": {}, \"gap_flag\": {}}}",
            p.frame_index, p.t_seconds, p.position.x, p.position.y, xm_field, ym_field, gap_flag
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
        let csv = export_csv(&path, None);
        let header = csv.lines().next().unwrap();
        assert_eq!(header, "frame_index,t_seconds,x_px,y_px,x_m,y_m,gap_flag");
    }

    #[test]
    fn csv_has_one_row_per_point() {
        let path = path_with_gap();
        let csv = export_csv(&path, None);
        // header + 2 rows + trailing newline -> 3 non-empty lines
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn csv_gap_flag_reflects_interpolated_source() {
        let path = path_with_gap();
        let csv = export_csv(&path, None);
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[1].ends_with(",false"));
        assert!(lines[2].ends_with(",true"));
    }

    #[test]
    fn csv_meter_columns_empty_without_calibration() {
        let path = path_with_gap();
        let csv = export_csv(&path, None);
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
        let csv = export_csv(&path, Some(&cal));
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
        let json = export_json(&path, None);
        assert!(json.trim_start().starts_with('['));
        assert!(json.trim_end().ends_with(']'));
        // Two objects -> exactly one comma separating them.
        assert_eq!(json.matches("frame_index").count(), 2);
    }

    #[test]
    fn json_meter_fields_null_without_calibration() {
        let path = path_with_gap();
        let json = export_json(&path, None);
        assert!(json.contains("\"x_m\": null"));
        assert!(json.contains("\"y_m\": null"));
    }

    #[test]
    fn json_gap_flag_reflects_interpolated_source() {
        let path = path_with_gap();
        let json = export_json(&path, None);
        assert!(json.contains("\"gap_flag\": false"));
        assert!(json.contains("\"gap_flag\": true"));
    }
}
