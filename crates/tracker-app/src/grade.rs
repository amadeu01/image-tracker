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
    accuracy::{grade, AccuracyReport, GroundTruthLabel, LabelStatus, PASS_FRACTION},
    Point, Sample, Source,
};

pub type GradeError = String;

/// A plate diameter must be a positive finite pixel count: it is a divisor
/// (errors are expressed in plate-diameters) and the tolerance scale.
fn check_plate_px(v: f64, what: &str) -> Result<f64, GradeError> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!(
            "{what}: plate_px must be a positive finite number of pixels, got {v}"
        ))
    }
}

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

/// Where the plate scale used for grading came from; printed in every report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlateSource {
    Manifest,
    Flag,
    Default,
}

#[derive(Debug, Clone)]
pub struct GradeArgs {
    pub points_csv: PathBuf,
    pub truth_csv: PathBuf,
    pub plate_diameter_px: f64,
    pub plate_source: PlateSource,
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
    let plate = check_plate_px(plate, &format!("clip `{clip}`"))?;
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
                check_plate_px(plate, "--plate-px")?;
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
                plate_source: if plate_explicit {
                    PlateSource::Flag
                } else {
                    PlateSource::Default
                },
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
                plate_source: if plate_explicit {
                    PlateSource::Flag
                } else {
                    PlateSource::Manifest
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

/// What the auto run reported for the lift (from the `<stem>.run.json` sidecar): `Ok(name)` or `Err(reason)` when
/// unknown / not recorded. An `Err` starting with `unknown:` means the run
/// itself said unknown (a MISS); any other `Err` means no data (UNKNOWN).
fn read_run_lift(points_csv: &Path) -> Result<String, String> {
    let sidecar = crate::run_info::sidecar_path(points_csv);
    let info = match crate::run_info::read_run_info(&sidecar) {
        Ok(Some(info)) => info,
        Ok(None) => return Err("no run info: export has no .run.json sidecar".into()),
        Err(e) => return Err(format!("unreadable run info: {e}")),
    };
    if let Some(rest) = info.lift.strip_prefix("unknown") {
        let reason = if info.lift_reason.is_empty() {
            rest.trim_start_matches([':', ' '])
        } else {
            info.lift_reason.as_str()
        };
        return Err(format!("unknown:{reason}"));
    }
    Ok(info.lift)
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
    let lift = read_run_lift(points_csv);
    let reps = read_run_reps(&sibling(points_csv, ".reps.json"));
    println!();
    println!("run checks ({})", exp.clip);
    println!("  LIFT TYPE            {}", lift_check(&exp.lift, &lift));
    println!("  REP COUNT            {}", rep_check(exp.reps, reps));
}

/// The plate-scale line shown at the top of every report.
fn plate_line(args: &GradeArgs) -> String {
    let source = match args.plate_source {
        PlateSource::Manifest => "manifest".to_string(),
        PlateSource::Flag => "--plate-px".to_string(),
        PlateSource::Default => format!("default {DEFAULT_PLATE_DIAMETER_PX}"),
    };
    format!("plate_px: {} (source: {source})", args.plate_diameter_px)
}

/// Manifest-mode pass-bar lines: coverage of visible labels, then PASS/MISS
/// on the coverage-adjusted within-tolerance fraction.
fn coverage_lines(r: &AccuracyReport) -> Vec<String> {
    let total = r.visible_labels();
    if total == 0 {
        return vec![
            "visible labels covered: 0/0".into(),
            "pass bar: UNKNOWN (no visible labels)".into(),
        ];
    }
    let pct = |n: usize| 100.0 * n as f64 / total as f64;
    let adjusted = r.within_count as f64 / total as f64 * 100.0;
    let verdict = if r.meets_pass_bar() == Some(true) {
        "PASS"
    } else {
        "MISS"
    };
    vec![
        format!(
            "visible labels covered: {}/{} ({:.1}%)",
            r.scored_frames,
            total,
            pct(r.scored_frames)
        ),
        format!(
            "pass bar: {verdict} ({adjusted:.1}% of visible labels within tolerance, need {:.0}%)",
            PASS_FRACTION * 100.0
        ),
    ]
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
    println!("  {}", plate_line(args));
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
    if args.expectation.is_some() {
        for line in coverage_lines(r) {
            println!("  {line}");
        }
    }
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

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grade-run-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn read_run_lift_reads_lift_and_reason_from_the_sidecar() {
        let d = temp_dir("ok");
        std::fs::write(
            d.join("c.run.json"),
            r#"{"lift":"deadlift","lift_reason":"r","verified":false,"seed_frame":1,"seed":[1.0,2.0],"detected_plate_px":80.0}"#,
        )
        .unwrap();
        assert_eq!(read_run_lift(&d.join("c.csv")), Ok("deadlift".to_string()));
        std::fs::write(
            d.join("u.run.json"),
            r#"{"lift":"unknown","lift_reason":"ambiguous","verified":false,"seed_frame":1,"seed":[1.0,2.0],"detected_plate_px":80.0}"#,
        )
        .unwrap();
        assert_eq!(
            read_run_lift(&d.join("u.csv")),
            Err("unknown:ambiguous".to_string())
        );
    }

    #[test]
    fn read_run_lift_without_sidecar_is_unknown_no_run_info() {
        let d = temp_dir("absent");
        let e = read_run_lift(&d.join("nope.csv")).unwrap_err();
        assert!(
            !e.starts_with("unknown:") && e.contains("no run info"),
            "{e}"
        );
    }

    #[test]
    fn read_run_lift_corrupt_sidecar_is_a_clear_message() {
        let d = temp_dir("bad");
        std::fs::write(d.join("c.run.json"), "{not json").unwrap();
        let e = read_run_lift(&d.join("c.csv")).unwrap_err();
        assert!(
            e.contains("c.run.json") && !e.starts_with("unknown:"),
            "{e}"
        );
    }

    #[test]
    fn non_positive_or_non_finite_plate_px_is_a_clear_error() {
        for bad in ["0", "-5", "NaN", "inf"] {
            let e = parse_grade_args(&s(&["p.csv", "t.csv", "--plate-px", bad])).unwrap_err();
            assert!(e.contains("plate"), "{bad}: {e}");
        }
        let m = temp_manifest("zeroplate", r#"{"v1": {"lift": "squat", "plate_px": 0}}"#);
        let e = parse_grade_args(&s(&[
            "--manifest",
            m.to_str().unwrap(),
            "--clip",
            "v1",
            "x.csv",
        ]))
        .unwrap_err();
        assert!(e.contains("v1") && e.contains("plate_px"), "{e}");
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use tracker_core::accuracy::grade as core_grade;

    fn args(source: PlateSource, plate: f64) -> GradeArgs {
        GradeArgs {
            points_csv: PathBuf::from("p.csv"),
            truth_csv: PathBuf::from("t.csv"),
            plate_diameter_px: plate,
            plate_source: source,
            tolerance_diameters: 0.1,
            expectation: None,
        }
    }

    fn report(seed_at: u64) -> AccuracyReport {
        let samples: Vec<Sample> = (seed_at..10)
            .map(|i| Sample {
                frame_index: i,
                position: Point::new(0.0, 0.0),
                source: Source::Tracked,
                confidence: Some(0.95),
            })
            .collect();
        let labels: Vec<GroundTruthLabel> = (0..10)
            .map(|i| GroundTruthLabel {
                frame_index: i,
                status: LabelStatus::Visible(Point::new(0.0, 0.0)),
            })
            .collect();
        core_grade(&samples, &labels, 5.0)
    }

    #[test]
    fn plate_line_names_the_source_of_the_scale() {
        assert_eq!(
            plate_line(&args(PlateSource::Default, 134.0)),
            "plate_px: 134 (source: default 134)"
        );
        assert_eq!(
            plate_line(&args(PlateSource::Manifest, 88.5)),
            "plate_px: 88.5 (source: manifest)"
        );
        assert_eq!(
            plate_line(&args(PlateSource::Flag, 100.0)),
            "plate_px: 100 (source: --plate-px)"
        );
    }

    #[test]
    fn plate_source_is_tracked_while_parsing() {
        let a = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let d = parse_grade_args(&a(&["p.csv", "t.csv"])).unwrap();
        assert_eq!(d.plate_source, PlateSource::Default);
        let f = parse_grade_args(&a(&["p.csv", "t.csv", "--plate-px", "9"])).unwrap();
        assert_eq!(f.plate_source, PlateSource::Flag);
    }

    #[test]
    fn late_seed_prints_coverage_and_a_miss() {
        let lines = coverage_lines(&report(5));
        assert_eq!(lines[0], "visible labels covered: 5/10 (50.0%)");
        assert!(
            lines[1].contains("MISS") && lines[1].contains("50.0%"),
            "{lines:?}"
        );
    }

    #[test]
    fn full_coverage_prints_a_pass() {
        let lines = coverage_lines(&report(0));
        assert_eq!(lines[0], "visible labels covered: 10/10 (100.0%)");
        assert!(lines[1].contains("PASS"), "{lines:?}");
    }

    #[test]
    fn no_visible_labels_prints_unknown_not_pass() {
        let lines = coverage_lines(&core_grade(&[], &[], 5.0));
        assert!(lines.iter().all(|l| !l.contains("PASS")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("UNKNOWN")), "{lines:?}");
    }
}
