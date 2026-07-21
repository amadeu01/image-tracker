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

#[derive(Debug, Clone)]
pub struct GradeArgs {
    pub points_csv: PathBuf,
    pub truth_csv: PathBuf,
    pub plate_diameter_px: f64,
    pub tolerance_diameters: f64,
}

pub fn parse_grade_args(args: &[String]) -> Result<GradeArgs, GradeError> {
    let mut positional: Vec<&String> = Vec::new();
    let mut plate = DEFAULT_PLATE_DIAMETER_PX;
    let mut tol = DEFAULT_TOLERANCE_DIAMETERS;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--plate-px" => {
                let v = args.get(i + 1).ok_or("--plate-px needs a value")?;
                plate = v.parse().map_err(|_| format!("bad --plate-px: {v}"))?;
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
    if positional.len() != 2 {
        return Err("expected <points.csv> <truth.csv>".into());
    }
    Ok(GradeArgs {
        points_csv: PathBuf::from(positional[0]),
        truth_csv: PathBuf::from(positional[1]),
        plate_diameter_px: plate,
        tolerance_diameters: tol,
    })
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
        out.push(Sample {
            frame_index,
            position: Point::new(x, y),
            source: if interpolated {
                Source::Interpolated
            } else {
                Source::Tracked
            },
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
    Ok(report)
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
    println!("  FALSE CONFIDENCE     {}  (bar absent, position reported as tracked)", r.false_confidence);
    println!("  coasted while absent {}", r.coasted_while_absent);
    println!("  correctly absent     {}", r.correctly_absent);
}
