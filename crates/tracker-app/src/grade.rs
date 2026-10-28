//! `tracker-app grade <points.csv> <truth.csv> [--plate-px N] [--tolerance-dia F]`
//! (PLAN 17.1): scores an exported Bar Path against hand-labelled ground
//! truth using `tracker_core::accuracy`.
//!
//! The CSV parsing lives here rather than in `tracker-core` so the core
//! crate stays dependency-free domain logic (CONTEXT.md / ADR layering):
//! `accuracy::grade` takes plain data and knows nothing about files.
//!
//! Points CSV: the `track`/GUI export (`frame_index,...,x_px,y_px,...,
//! gap_flag,...`). `gap_flag` distinguishes a directly-tracked sample from
//! an interpolated one, which is what separates "false confidence" from
//! "coasted while absent".
//!
//! Truth CSV: `video,frame_index,x_px,y_px,status,target` as written by
//! `groundtruth/label.html`. Blank x/y with a non-visible status is normal
//! and expected — those are the frames that test whether the tracker knows
//! when to say nothing.

use std::path::{Path, PathBuf};

use tracker_core::{
    accuracy::{grade, AccuracyReport, GroundTruthLabel, LabelStatus},
    Point, Sample, Source,
};

pub type GradeError = String;

/// Apparent plate diameter in pixels, used to express errors in
/// plate-diameters. Default measured on the v3/v4 rig (464x832 footage):
/// the circle fit in `docs/design/tracking-audit-2026-07-21.md` put the
/// plate radius at ~67px across frames.
pub const DEFAULT_PLATE_DIAMETER_PX: f64 = 134.0;

/// "On the bar" tolerance, as a fraction of a plate diameter. 0.1 of a
/// 0.450m plate is 45mm — tight enough that a lock on adjacent rack
/// hardware fails it, loose enough to absorb ~5px of human labelling
/// uncertainty.
pub const DEFAULT_TOLERANCE_DIAMETERS: f64 = 0.1;

/// Owner-supplied expectations for one clip, read from the manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipExpectation {
    pub clip: String,
    pub lift: String,
    pub reps: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct GradeArgs {
    pub points_csv: PathBuf,
    pub truth_csv: PathBuf,
    pub plate_diameter_px: f64,
    pub tolerance_diameters: f64,
    /// Present only when `--manifest`/`--clip` were given.
    pub expectation: Option<ClipExpectation>,
}

/// Looks up `clip` in the manifest JSON text. A null/missing `plate_px` is an
/// error (never the 134 default): the owner must measure it.
fn lookup_clip(manifest_text: &str, clip: &str) -> Result<(ClipExpectation, f64), GradeError> {
    let v: serde_json::Value =
        serde_json::from_str(manifest_text).map_err(|e| format!("bad manifest JSON: {e}"))?;
    let obj = v.as_object().ok_or("manifest must be a JSON object")?;
    let Some(entry) = obj.get(clip) else {
        let mut known: Vec<&str> = obj.keys().map(String::as_str).collect();
        known.sort_unstable();
        return Err(format!(
            "clip `{clip}` not in manifest; known clips: {}",
            known.join(", ")
        ));
    };
    let plate = entry
        .get("plate_px")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| {
            format!(
                "clip `{clip}`: plate_px is not set in the manifest; the owner must fill plate_px (no default is assumed)"
            )
        })?;
    let lift = entry
        .get("lift")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("clip `{clip}`: manifest entry has no `lift`"))?
        .to_string();
    let reps = entry.get("reps").and_then(serde_json::Value::as_u64);
    Ok((
        ClipExpectation {
            clip: clip.to_string(),
            lift,
            reps,
        },
        plate,
    ))
}

pub fn parse_grade_args(args: &[String]) -> Result<GradeArgs, GradeError> {
    let mut positional: Vec<&String> = Vec::new();
    let mut plate = DEFAULT_PLATE_DIAMETER_PX;
    let mut plate_explicit = false;
    let mut tol = DEFAULT_TOLERANCE_DIAMETERS;
    let mut manifest: Option<PathBuf> = None;
    let mut clip: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--plate-px" => {
                let v = args.get(i + 1).ok_or("--plate-px needs a value")?;
                plate = v.parse().map_err(|_| format!("bad --plate-px: {v}"))?;
                plate_explicit = true;
                i += 2;
            }
            "--manifest" => {
                let v = args.get(i + 1).ok_or("--manifest needs a value")?;
                manifest = Some(PathBuf::from(v));
                i += 2;
            }
            "--clip" => {
                let v = args.get(i + 1).ok_or("--clip needs a value")?;
                clip = Some(v.clone());
                i += 2;
            }
            "--tolerance-dia" => {
                let v = args.get(i + 1).ok_or("--tolerance-dia needs a value")?;
                tol = v.parse().map_err(|_| format!("bad --tolerance-dia: {v}"))?;
                i += 2;
            }
            _ => {
                positional.push(&args[i]);
                i += 1;
            }
        }
    }
    match (manifest, clip) {
        (None, None) => {
            if positional.len() != 2 {
                return Err("expected <points.csv> <truth.csv>".into());
            }
            Ok(GradeArgs {
                points_csv: PathBuf::from(positional[0]),
                truth_csv: PathBuf::from(positional[1]),
                plate_diameter_px: plate,
                tolerance_diameters: tol,
                expectation: None,
            })
        }
        (Some(manifest), Some(clip)) => {
            if positional.is_empty() || positional.len() > 2 {
                return Err("expected <points.csv> [<truth.csv>]".into());
            }
            let text = std::fs::read_to_string(&manifest)
                .map_err(|e| format!("{}: {e}", manifest.display()))?;
            let (expectation, manifest_plate) = lookup_clip(&text, &clip)?;
            let truth_csv = match positional.get(1) {
                Some(p) => PathBuf::from(p),
                None => manifest
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!("{clip}.csv")),
            };
            Ok(GradeArgs {
                points_csv: PathBuf::from(positional[0]),
                truth_csv,
                // An explicit --plate-px is a deliberate user override.
                plate_diameter_px: if plate_explicit {
                    plate
                } else {
                    manifest_plate
                },
                tolerance_diameters: tol,
                expectation: Some(expectation),
            })
        }
        _ => Err("--manifest and --clip must be given together".into()),
    }
}

/// Reads an exported points CSV into `Sample`s.
pub fn read_points(path: &Path) -> Result<Vec<Sample>, GradeError> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let header = lines.next().ok_or("points CSV is empty")?;
    let cols: Vec<&str> = header.split(',').map(str::trim).collect();
    let idx = |name: &str| -> Result<usize, GradeError> {
        cols.iter()
            .position(|c| *c == name)
            .ok_or_else(|| format!("points CSV missing column `{name}`"))
    };
    let (fi, xi, yi) = (idx("frame_index")?, idx("x_px")?, idx("y_px")?);
    let gi = idx("gap_flag").ok();
    let ci = idx("confidence").ok();

    let mut out = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("");
        let frame_index: u64 = get(fi)
            .parse()
            .map_err(|_| format!("row {}: bad frame_index `{}`", n + 2, get(fi)))?;
        let (Ok(x), Ok(y)) = (get(xi).parse::<f64>(), get(yi).parse::<f64>()) else {
            continue;
        };
        // gap_flag true == this position was interpolated across a gap.
        let interpolated = gi.is_some_and(|i| matches!(get(i), "true" | "1"));
        let confidence = ci.and_then(|i| get(i).parse::<f64>().ok());
        out.push(Sample {
            frame_index,
            position: Point::new(x, y),
            source: if interpolated {
                Source::Interpolated
            } else {
                Source::Tracked
            },
            confidence,
        });
    }
    Ok(out)
}

/// Reads a ground-truth CSV into labels.
pub fn read_truth(path: &Path) -> Result<Vec<GroundTruthLabel>, GradeError> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let header = lines.next().ok_or("truth CSV is empty")?;
    let cols: Vec<&str> = header.split(',').map(str::trim).collect();
    let idx = |name: &str| -> Result<usize, GradeError> {
        cols.iter()
            .position(|c| *c == name)
            .ok_or_else(|| format!("truth CSV missing column `{name}`"))
    };
    let (fi, xi, yi, si) = (
        idx("frame_index")?,
        idx("x_px")?,
        idx("y_px")?,
        idx("status")?,
    );

    let mut out = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("");
        let frame_index: u64 = get(fi)
            .parse()
            .map_err(|_| format!("row {}: bad frame_index `{}`", n + 2, get(fi)))?;
        let pos = match (get(xi).parse::<f64>(), get(yi).parse::<f64>()) {
            (Ok(x), Ok(y)) => Some(Point::new(x, y)),
            _ => None,
        };
        let status = match (get(si), pos) {
            ("visible", Some(p)) => LabelStatus::Visible(p),
            ("blurred", Some(p)) => LabelStatus::Blurred(p),
            ("occluded", _) => LabelStatus::Occluded,
            ("out_of_frame", _) => LabelStatus::OutOfFrame,
            (s, None) => return Err(format!("row {}: status `{s}` needs x/y", n + 2)),
            (s, _) => return Err(format!("row {}: unknown status `{s}`", n + 2)),
        };
        out.push(GroundTruthLabel {
            frame_index,
            status,
        });
    }
    Ok(out)
}

pub fn run_grade(args: GradeArgs) -> Result<AccuracyReport, GradeError> {
    let samples = read_points(&args.points_csv)?;
    let labels = read_truth(&args.truth_csv)?;
    let tolerance_px = args.tolerance_diameters * args.plate_diameter_px;
    let report = grade(&samples, &labels, tolerance_px);
    print_report(&report, &args);
    if let Some(exp) = &args.expectation {
        print_run_checks(&args.points_csv, exp);
    }
    Ok(report)
}

/// `<stem>.csv` -> `<stem><suffix>` next to it.
fn sibling(points_csv: &Path, suffix: &str) -> PathBuf {
    points_csv.with_extension(suffix.trim_start_matches('.'))
}

/// What the auto run reported for the lift: `Ok(name)` or `Err(reason)` when
/// unknown / not recorded. An `Err` starting with `unknown:` means the run
/// itself said unknown (a MISS); any other `Err` means no data (UNKNOWN).
fn read_run_lift(json_path: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(json_path).map_err(|_| format!("no {}", json_path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "bad export json".to_string())?;
    let Some(lift) = v.get("lift").and_then(serde_json::Value::as_str) else {
        return Err("export has no lift field".into());
    };
    if let Some(rest) = lift.strip_prefix("unknown") {
        let rest = rest.trim_start_matches([':', ' ']);
        let reason = v
            .get("lift_reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(rest);
        return Err(format!("unknown:{reason}"));
    }
    Ok(lift.to_string())
}

/// Result line for the LIFT TYPE check.
pub fn lift_check(expected: &str, run: &Result<String, String>) -> String {
    match run {
        Ok(got) if got == expected => "PASS".into(),
        Ok(got) => format!("MISS (expected {expected}, got {got})"),
        Err(e) if e.starts_with("unknown:") => {
            let reason = e["unknown:".len()..].trim();
            let reason = if reason.is_empty() {
                "no reason given"
            } else {
                reason
            };
            format!("MISS (unknown: {reason})")
        }
        Err(e) => format!("UNKNOWN ({e})"),
    }
}

/// Result line for the REP COUNT check.
pub fn rep_check(expected: Option<u64>, got: Option<usize>) -> String {
    match (expected, got) {
        (None, _) => "UNKNOWN (manifest reps not filled)".into(),
        (Some(_), None) => "UNKNOWN (run has no .reps.json)".into(),
        (Some(e), Some(g)) if e == g as u64 => "PASS".into(),
        (Some(e), Some(g)) => format!("MISS (expected {e}, got {g})"),
    }
}

fn read_run_reps(path: &Path) -> Option<usize> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.as_array().map(Vec::len)
}

fn print_run_checks(points_csv: &Path, exp: &ClipExpectation) {
    let lift = read_run_lift(&sibling(points_csv, ".json"));
    let reps = read_run_reps(&sibling(points_csv, ".reps.json"));
    println!();
    println!("run checks ({})", exp.clip);
    println!("  LIFT TYPE            {}", lift_check(&exp.lift, &lift));
    println!("  REP COUNT            {}", rep_check(exp.reps, reps));
}

pub fn print_report(r: &AccuracyReport, args: &GradeArgs) {
    let fmt = |v: Option<f64>| match v {
        Some(x) => format!("{x:.1}"),
        None => "—".into(),
    };
    let dia = |v: Option<f64>| match v {
        Some(x) => format!("{:.3}", x / args.plate_diameter_px),
        None => "—".into(),
    };
    println!();
    println!("accuracy vs ground truth ({})", args.truth_csv.display());
    println!("  scored frames        {}", r.scored_frames);
    println!("  unmatched labels     {}", r.unmatched_frames);
    println!(
        "  mean error           {} px  ({} plate-dia)",
        fmt(r.mean_error_px),
        dia(r.mean_error_px)
    );
    println!(
        "  p95 error            {} px  ({} plate-dia)",
        fmt(r.p95_error_px),
        dia(r.p95_error_px)
    );
    println!(
        "  max error            {} px  ({} plate-dia)",
        fmt(r.max_error_px),
        dia(r.max_error_px)
    );
    println!(
        "  within {:.2} plate-dia  {}",
        args.tolerance_diameters,
        match r.within_tolerance {
            Some(f) => format!("{:.0}%", f * 100.0),
            None => "—".into(),
        }
    );
    println!(
        "  FALSE CONFIDENCE     {}  (bar absent, confident position reported)",
        r.false_confidence
    );
    println!(
        "  suspect while absent {}  (bar absent, low-confidence position)",
        r.suspect_while_absent
    );
    println!("  coasted while absent {}", r.coasted_while_absent);
    println!("  correctly absent     {}", r.correctly_absent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn temp_manifest(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grade-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("clips.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    const MANIFEST: &str = r#"{
      "v5": {"video": "a.mp4", "lift": "deadlift", "reps": 3, "plate_px": 88.5},
      "v6": {"video": "b.mp4", "lift": "squat", "reps": null, "plate_px": null}
    }"#;

    #[test]
    fn manifest_plate_px_overrides_the_default() {
        let m = temp_manifest("plate", MANIFEST);
        let a = parse_grade_args(&s(&[
            "--manifest",
            m.to_str().unwrap(),
            "--clip",
            "v5",
            "x.csv",
        ]))
        .unwrap();
        assert_eq!(a.plate_diameter_px, 88.5);
        assert_eq!(a.truth_csv, m.parent().unwrap().join("v5.csv"));
        let e = a.expectation.unwrap();
        assert_eq!((e.lift.as_str(), e.reps), ("deadlift", Some(3)));
    }

    #[test]
    fn missing_plate_px_in_manifest_is_a_clear_error_not_the_134_default() {
        let m = temp_manifest("noplate", MANIFEST);
        let err = parse_grade_args(&s(&[
            "--manifest",
            m.to_str().unwrap(),
            "--clip",
            "v6",
            "x.csv",
        ]))
        .unwrap_err();
        assert!(err.contains("v6") && err.contains("fill plate_px"), "{err}");
    }

    #[test]
    fn unknown_clip_id_lists_known_clips() {
        let m = temp_manifest("unk", MANIFEST);
        let err = parse_grade_args(&s(&[
            "--manifest",
            m.to_str().unwrap(),
            "--clip",
            "v9",
            "x.csv",
        ]))
        .unwrap_err();
        assert!(err.contains("v9") && err.contains("v5, v6"), "{err}");
    }

    #[test]
    fn unknown_lift_is_reported_as_miss_with_reason() {
        let r = Err("unknown: ambiguous trajectory".to_string());
        assert_eq!(
            lift_check("deadlift", &r),
            "MISS (unknown: ambiguous trajectory)"
        );
        let none = Err("export has no lift field".to_string());
        assert_eq!(
            lift_check("deadlift", &none),
            "UNKNOWN (export has no lift field)"
        );
    }

    #[test]
    fn lift_mismatch_is_reported() {
        assert_eq!(lift_check("deadlift", &Ok("deadlift".into())), "PASS");
        assert_eq!(
            lift_check("deadlift", &Ok("squat".into())),
            "MISS (expected deadlift, got squat)"
        );
    }

    #[test]
    fn rep_count_mismatch_is_reported() {
        assert_eq!(rep_check(Some(5), Some(5)), "PASS");
        assert_eq!(rep_check(Some(5), Some(4)), "MISS (expected 5, got 4)");
    }

    #[test]
    fn rep_count_unknown_when_manifest_reps_null() {
        assert_eq!(
            rep_check(None, Some(4)),
            "UNKNOWN (manifest reps not filled)"
        );
    }

    #[test]
    fn existing_positional_grade_invocation_is_unchanged() {
        let a = parse_grade_args(&s(&["p.csv", "t.csv", "--plate-px", "100"])).unwrap();
        assert_eq!(a.points_csv, PathBuf::from("p.csv"));
        assert_eq!(a.truth_csv, PathBuf::from("t.csv"));
        assert_eq!(a.plate_diameter_px, 100.0);
        assert!(a.expectation.is_none());
        let d = parse_grade_args(&s(&["p.csv", "t.csv"])).unwrap();
        assert_eq!(d.plate_diameter_px, DEFAULT_PLATE_DIAMETER_PX);
        assert!(parse_grade_args(&s(&["p.csv"])).is_err());
    }
}
