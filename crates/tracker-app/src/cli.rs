//! Headless CLI mode (task 3.4): `tracker-app track <video> --seed-frame N
//! --seed X,Y --out <dir>` runs the same tracking pipeline the GUI drives
//! (2.6's `TemplateTracker`/`TrackingSession`, sequential `FfmpegFrameSource`)
//! without an egui window, then writes the overlay MP4 (3.1 `render_overlay`
//! + 3.2 `FfmpegVideoSink`) and CSV/JSON exports (3.3) to `--out`.
//!
//! This exists so the pipeline can be exercised end-to-end against every
//! `test_videos/` clip from a script, without a human driving the GUI for
//! each one.

use std::path::PathBuf;

use tracker_core::{
    all_rep_metrics, export_csv, export_json, export_reps_csv, export_reps_json, hue_histogram,
    recommend_marker_hues, segment_reps_for_lift, velocity_series, Calibration, HueHistogramConfig,
    LiftType, Point, Preprocessor, Source,
};

use crate::ffprobe;
use crate::frame_cache::FrameDecoder;
use crate::overlay_export::render_overlay_video;
use crate::seek_source::SeekingFrameDecoder;
use crate::tracking;

/// Parsed `track` subcommand arguments.
#[derive(Debug)]
pub struct TrackArgs {
    pub video_path: PathBuf,
    /// `--seed-frame`/`--seed` are given together (manual) or not at all
    /// (auto: the pre-pass proposes the seed and the lift type).
    pub seed_frame: Option<u64>,
    pub seed: Option<Point>,
    pub out_dir: PathBuf,
    /// Optional tracker tuning overrides (task 3.6): `--patch-radius`,
    /// `--search-radius`, `--min-score`, `--update-threshold`,
    /// `--coast-limit`, `--reacquire-min-score`, and (11.3) a repeatable
    /// `--filter gaussian:<sigma>`/`--filter median:<k>` chain, applied in
    /// flag order to both the template and color tracker paths. Unset
    /// fields fall back to `tracking`'s defaults; an absent `--filter`
    /// leaves the chain empty (no filtering, unchanged from before 11.3).
    pub tuning: tracking::TrackerTuning,
    /// `--tracker auto|template|color|circle` (task 4.3, 17.5): which
    /// tracker to run. Defaults to `Auto` (suggest from the seed patch);
    /// `circle` (the plate-circle tracker, audit F6) is never chosen by
    /// `Auto` and must be selected explicitly.
    pub tracker_selection: tracking::TrackerSelection,
    /// Optional calibration (task 5.4): `--cal x1,y1,x2,y2` (two points)
    /// plus `--cal-length-m <meters>`, the known real-world distance
    /// between them. When both are given, velocity/rep metrics are
    /// computed in m/s/meters instead of px/s/px.
    pub cal_points: Option<(Point, Point)>,
    pub cal_length_m: Option<f64>,
}

/// Everything that can go wrong parsing CLI args, probing, tracking, or
/// writing outputs — collapsed to a single string for `main`'s exit path.
pub type CliError = String;

/// Parses one `--filter` argument's value (11.3): `gaussian:<sigma>` or
/// `median:<k>`. Repeatable on the command line; `parse_track_args` collects
/// each occurrence in order into a `PreprocessorChain`, so `--filter
/// median:3 --filter gaussian:1.5` filters median-then-gaussian, matching
/// flag order exactly (this is also the shared parser 11.4's `compare`
/// subcommand will reuse for its own `--filter` flags).
pub fn parse_filter_spec(spec: &str) -> Result<Preprocessor, CliError> {
    let (kind, param) = spec.split_once(':').ok_or_else(|| {
        format!("bad --filter: expected gaussian:<sigma> or median:<k>, got {spec}")
    })?;
    match kind {
        "gaussian" => {
            let sigma: f64 = param
                .trim()
                .parse()
                .map_err(|_| format!("bad --filter: expected gaussian:<sigma>, got {spec}"))?;
            Ok(Preprocessor::GaussianBlur { sigma })
        }
        "median" => {
            let k: u32 = param
                .trim()
                .parse()
                .map_err(|_| format!("bad --filter: expected median:<k>, got {spec}"))?;
            Ok(Preprocessor::Median { k })
        }
        _ => Err(format!(
            "bad --filter: expected gaussian:<sigma> or median:<k>, got {spec}"
        )),
    }
}

/// Parses `track <video> --seed-frame N --seed X,Y --out <dir>` from the
/// args following the subcommand name itself.
pub fn parse_track_args(args: &[String]) -> Result<TrackArgs, CliError> {
    let mut video_path: Option<PathBuf> = None;
    let mut seed_frame: Option<u64> = None;
    let mut seed: Option<Point> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut tuning = tracking::TrackerTuning::default();
    let mut tracker_selection = tracking::TrackerSelection::default();
    let mut cal_points: Option<(Point, Point)> = None;
    let mut cal_length_m: Option<f64> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seed-frame" => {
                let v = args.get(i + 1).ok_or("--seed-frame needs a value")?;
                seed_frame = Some(v.parse().map_err(|_| format!("bad --seed-frame: {v}"))?);
                i += 2;
            }
            "--seed" => {
                let v = args.get(i + 1).ok_or("--seed needs a value (X,Y)")?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| format!("bad --seed (expected X,Y): {v}"))?;
                let x: f64 = x.trim().parse().map_err(|_| format!("bad --seed x: {v}"))?;
                let y: f64 = y.trim().parse().map_err(|_| format!("bad --seed y: {v}"))?;
                seed = Some(Point::new(x, y));
                i += 2;
            }
            "--out" => {
                let v = args.get(i + 1).ok_or("--out needs a value")?;
                out_dir = Some(PathBuf::from(v));
                i += 2;
            }
            "--patch-radius" => {
                let v = args.get(i + 1).ok_or("--patch-radius needs a value")?;
                tuning.patch_radius =
                    Some(v.parse().map_err(|_| format!("bad --patch-radius: {v}"))?);
                i += 2;
            }
            "--search-radius" => {
                let v = args.get(i + 1).ok_or("--search-radius needs a value")?;
                tuning.search_radius =
                    Some(v.parse().map_err(|_| format!("bad --search-radius: {v}"))?);
                i += 2;
            }
            "--min-score" => {
                let v = args.get(i + 1).ok_or("--min-score needs a value")?;
                tuning.min_score = Some(v.parse().map_err(|_| format!("bad --min-score: {v}"))?);
                i += 2;
            }
            "--update-threshold" => {
                let v = args.get(i + 1).ok_or("--update-threshold needs a value")?;
                tuning.update_threshold = Some(
                    v.parse()
                        .map_err(|_| format!("bad --update-threshold: {v}"))?,
                );
                i += 2;
            }
            "--anchor-floor" => {
                let v = args.get(i + 1).ok_or("--anchor-floor needs a value")?;
                tuning.anchor_floor =
                    Some(v.parse().map_err(|_| format!("bad --anchor-floor: {v}"))?);
                i += 2;
            }
            "--coast-limit" => {
                let v = args.get(i + 1).ok_or("--coast-limit needs a value")?;
                tuning.coast_limit =
                    Some(v.parse().map_err(|_| format!("bad --coast-limit: {v}"))?);
                i += 2;
            }
            "--reacquire-min-score" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--reacquire-min-score needs a value")?;
                tuning.reacquire_min_score = Some(
                    v.parse()
                        .map_err(|_| format!("bad --reacquire-min-score: {v}"))?,
                );
                i += 2;
            }
            "--tracker" => {
                let v = args.get(i + 1).ok_or("--tracker needs a value")?;
                tracker_selection = v.parse()?;
                i += 2;
            }
            "--filter" => {
                let v = args.get(i + 1).ok_or("--filter needs a value")?;
                tuning.preprocessor.push(parse_filter_spec(v)?);
                i += 2;
            }
            "--cal" => {
                let v = args.get(i + 1).ok_or("--cal needs a value (x1,y1,x2,y2)")?;
                let parts: Vec<&str> = v.split(',').collect();
                if parts.len() != 4 {
                    return Err(format!("bad --cal (expected x1,y1,x2,y2): {v}"));
                }
                let nums: Result<Vec<f64>, _> = parts.iter().map(|p| p.trim().parse()).collect();
                let nums: Vec<f64> = nums.map_err(|_| format!("bad --cal (non-numeric): {v}"))?;
                cal_points = Some((Point::new(nums[0], nums[1]), Point::new(nums[2], nums[3])));
                i += 2;
            }
            "--cal-length-m" => {
                let v = args.get(i + 1).ok_or("--cal-length-m needs a value")?;
                cal_length_m = Some(v.parse().map_err(|_| format!("bad --cal-length-m: {v}"))?);
                i += 2;
            }
            other if video_path.is_none() && !other.starts_with("--") => {
                video_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    if seed_frame.is_some() != seed.is_some() {
        return Err(
            "--seed-frame and --seed must be given together (or neither, to auto-detect the bar)"
                .into(),
        );
    }

    Ok(TrackArgs {
        video_path: video_path.ok_or("missing <video> argument")?,
        seed_frame,
        seed,
        out_dir: out_dir.ok_or("missing --out")?,
        tuning,
        tracker_selection,
        cal_points,
        cal_length_m,
    })
}

/// What the auto pre-pass decided, beyond the seed itself.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoSeedInfo {
    pub lift_reason: String,
    pub plate_px: f64,
    pub confidence: f64,
}

/// The seed a run tracks from, plus the lift it implies. `auto` is `Some`
/// only when the pre-pass proposed the seed (such a run is unverified).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSeed {
    pub frame_index: u64,
    pub position: Point,
    pub lift: LiftType,
    pub auto: Option<AutoSeedInfo>,
}

impl ResolvedSeed {
    /// A user-supplied seed: lift unknown (today's behaviour), no provenance.
    pub fn manual(frame_index: u64, position: Point) -> Self {
        Self {
            frame_index,
            position,
            lift: LiftType::Unknown,
            auto: None,
        }
    }
}

/// Resolves the seed for a run. `Some(manual)` is used as-is without
/// touching the decoder; `None` runs the unseeded pre-pass over
/// `frame_count` frames and accepts its proposal. No proposal is a clean
/// error, never a panic.
pub fn resolve_seed<D>(
    manual: Option<(u64, Point)>,
    decoder: &mut D,
    frame_count: u64,
    on_progress: impl FnMut(u64, u64),
) -> Result<ResolvedSeed, CliError>
where
    D: FrameDecoder<Frame = tracker_core::Frame>,
    D::Error: std::fmt::Display,
{
    if let Some((frame_index, position)) = manual {
        return Ok(ResolvedSeed::manual(frame_index, position));
    }
    let proposal = crate::prepass::run_prepass(
        decoder,
        frame_count,
        crate::prepass::DEFAULT_PREPASS_STRIDE,
        &std::sync::atomic::AtomicBool::new(false),
        on_progress,
    )?;
    let Some(p) = proposal else {
        return Err(
            "no bar could be found automatically (no moving plate-like circle); \
             pass --seed-frame and --seed to track manually"
                .into(),
        );
    };
    Ok(ResolvedSeed {
        frame_index: p.frame_index,
        position: p.position,
        lift: p.lift.lift,
        auto: Some(AutoSeedInfo {
            lift_reason: p.lift.reason,
            plate_px: p.plate_diameter_px,
            confidence: p.confidence,
        }),
    })
}

/// Writes `<stem>.run.json` for auto runs. Manual runs write nothing
/// (`Ok(None)`) and remove a stale sidecar of the same stem left by an
/// earlier auto run, so `grade` never reads a lift for a seeded run. A
/// missing sidecar reads back as "verified".
pub fn write_run_sidecar(
    out_dir: &std::path::Path,
    stem: &str,
    seed: &ResolvedSeed,
) -> Result<Option<PathBuf>, CliError> {
    let path = out_dir.join(format!("{stem}.run.json"));
    let Some(auto) = &seed.auto else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to remove stale {}: {e}", path.display())),
        }
        return Ok(None);
    };
    crate::run_info::write_run_info(
        &path,
        &crate::run_info::RunInfo {
            lift: crate::run_info::lift_name(seed.lift).to_string(),
            lift_reason: auto.lift_reason.clone(),
            verified: false,
            seed_frame: seed.frame_index,
            seed: [seed.position.x, seed.position.y],
            plate_px: auto.plate_px,
        },
    )?;
    Ok(Some(path))
}

/// Rep segmentation honouring the lift's phase order (a deadlift is
/// concentric-first); `Unknown` keeps the eccentric-first default.
fn segment_run_reps(
    lift: LiftType,
    velocity: &[tracker_core::VelocitySample],
    calibrated: bool,
) -> Vec<tracker_core::Rep> {
    segment_reps_for_lift(
        lift,
        velocity,
        tracking::rep_segmentation_config(calibrated),
    )
}

/// Runs the `track` subcommand: probe -> track (blocking, on this thread) ->
/// render overlay MP4 -> write CSV/JSON, all under `args.out_dir`.
#[tracing::instrument(skip_all, fields(video = %args.video_path.display(), out_dir = %args.out_dir.display()))]
pub fn run_track(args: TrackArgs) -> Result<(), CliError> {
    let metadata = ffprobe::probe(&args.video_path)
        .map_err(|e| format!("failed to probe {}: {e}", args.video_path.display()))?;

    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("failed to create out dir {}: {e}", args.out_dir.display()))?;

    let manual = args.seed_frame.zip(args.seed);
    let seed = if let Some((frame_index, position)) = manual {
        ResolvedSeed::manual(frame_index, position)
    } else {
        let frame_count = metadata.frame_count.ok_or_else(|| {
            "could not determine the video's frame count for auto-detection; \
             pass --seed-frame and --seed manually"
                .to_string()
        })?;
        let mut decoder = SeekingFrameDecoder::new(
            args.video_path.clone(),
            metadata.display_width(),
            metadata.display_height(),
            metadata.fps_num,
            metadata.fps_den,
        );
        let mut last_decile = 0;
        let seed = resolve_seed(None, &mut decoder, frame_count, |done, total| {
            let decile = done * 10 / total.max(1);
            if decile > last_decile {
                last_decile = decile;
                eprintln!("pre-pass: {}% ({done}/{total} samples)", decile * 10);
            }
        })?;
        if let Some(auto) = &seed.auto {
            println!(
                "lift: {} ({})",
                crate::run_info::lift_name(seed.lift),
                auto.lift_reason
            );
            println!(
                "seed proposal: frame {} at ({:.1}, {:.1}), plate {:.1} px, confidence {:.2}",
                seed.frame_index, seed.position.x, seed.position.y, auto.plate_px, auto.confidence
            );
            println!("UNVERIFIED: seed was auto-proposed; confirm or pass --seed-frame/--seed");
        }
        seed
    };

    let handle = tracking::spawn_tracking(tracking::TrackingJob {
        video_path: args.video_path.clone(),
        width: metadata.display_width(),
        height: metadata.display_height(),
        fps_num: metadata.fps_num,
        fps_den: metadata.fps_den,
        seed_frame_index: seed.frame_index,
        seed_position: seed.position,
        tracker_config: tracking::tracker_config(args.tuning.clone()),
        session_config: tracking::session_config(args.tuning.clone()),
        tracker_selection: args.tracker_selection,
        color_tracker_config: tracking::color_tracker_config(args.tuning.clone()),
    });

    // Headless: no UI to place a new seed on NeedsReseed. Best-effort
    // auto-recovery instead of giving up: resume from the last known
    // position at the frame the session paused on. This is a worse seed
    // than a human would pick (no re-examination of the frame), but it lets
    // a single CLI run produce a full path + honest reseed-event count for
    // judging tracker quality end-to-end, rather than truncating the run at
    // the first loss.
    let mut run_state = tracking::TrackingRunState::started();
    let mut reseed_events: u64 = 0;
    // recv() Err means the worker thread exited without sending Done/Error.
    while let Ok(msg) = handle.messages.recv() {
        let needs_reseed = matches!(
            &msg,
            tracking::TrackingMessage::Progress {
                state: tracker_core::SessionState::NeedsReseed,
                ..
            }
        );
        let done = run_state.apply(msg);
        if done {
            break;
        }
        if needs_reseed {
            // 10.9: resume at the *current* paused frame
            // (`last_frame_index`, now sourced from
            // `TrackingSession::frame_index()` rather than the last
            // recorded sample) using the last *tracked* (not
            // coasted/interpolated) position -- `last_tracked_position`
            // (10.2) -- rather than `last_position`, which during a gap is
            // wherever the linear interpolation toward coasted-garbage last
            // landed (e.g. the rack, y=12). Reseeding onto interpolated
            // garbage was silently producing a worse-than-nothing new seed;
            // reseeding at a stale frame index was worse still: it fed the
            // same already-recorded frame back into `reseed` on every
            // iteration, which (before the `TrackingSession::reseed`
            // monotonic-samples fix) minted duplicate/regressing samples
            // and effectively never advanced past that frame.
            let (Some(idx), Some(pos)) =
                (run_state.last_frame_index, run_state.last_tracked_position)
            else {
                break; // shouldn't happen: Progress always sets both once tracking has started
            };
            reseed_events += 1;
            tracing::warn!(
                video_frame_index = idx,
                x = pos.x,
                y = pos.y,
                reseed_events,
                "headless auto-resume: reseeding from last *tracked* position"
            );
            handle.resume(idx, pos);
        }
    }

    // 17.4b: `Lost` is terminal — the worker ends the run itself (see
    // `tracking::run_tracking_loop`) rather than pausing for a reseed, so
    // by the time we get here the loop above has already fallen through to
    // `Done` with whatever partial path it collected. Report that
    // distinctly from a normal reseed-recovery completion: this is the
    // honest "the run gave up, here's what it saw before that" case the
    // audit asked for (F4/F5), not "tracked cleanly to the end".
    if run_state.session_state == Some(tracker_core::SessionState::Lost) {
        eprintln!(
            "warning: tracking lost the object at frame {} (sustained low identity confidence) \
             and stopped rather than auto-resuming from an untrustworthy position; \
             exporting the partial path collected up to that frame ({reseed_events} reseed \
             event(s) before that)",
            run_state.last_frame_index.unwrap_or_default()
        );
        tracing::warn!(
            video_frame_index = run_state.last_frame_index,
            reseed_events,
            "run ended: tracking lost, not auto-resumed"
        );
    }

    if let Some(err) = &run_state.error {
        return Err(format!("tracking error: {err}"));
    }
    let Some(bar_path) = run_state.bar_path.clone() else {
        return Err(format!(
            "tracking worker exited without completing (after {} frame(s) processed, {reseed_events} reseed event(s))",
            run_state.frames_processed
        ));
    };

    let stem = args
        .video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("out");

    // Optional calibration (task 5.4): both `--cal` and `--cal-length-m`
    // must be given; a mismatched pair (one but not the other) is treated
    // as "no calibration" rather than an error, since the CLI's job is to
    // produce best-effort output, not fail a whole run over one flag.
    let cal = match (args.cal_points, args.cal_length_m) {
        (Some((a, b)), Some(len)) => match Calibration::new(a, b, len) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("ignoring --cal: {e}");
                None
            }
        },
        _ => None,
    };

    // Velocity/rep metrics (task 5.2-5.4): best-effort -- a bar path too
    // short to differentiate (e.g. a single point) just yields no
    // velocity/reps rather than failing the whole run. But a failure here
    // must never be *silent* (10.9): before this fix, `.ok()` swallowed
    // `VelocityError` outright, so a reseed-produced duplicate/regressing
    // frame index (see `TrackingSession::reseed`) tripped
    // `NonMonotonicTime` and the run finished "successfully" with a
    // header-only reps.csv and no indication anything had gone wrong.
    let velocity = match velocity_series(bar_path.points(), 5, cal.as_ref()) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!(
                "warning: could not compute velocity/reps for {}: {e} \
                 (positions are still exported; velocity columns and reps.csv will be empty)",
                args.video_path.display()
            );
            tracing::error!(
                error = %e,
                points = bar_path.points().len(),
                "velocity_series failed; exporting positions only, no velocity/reps"
            );
            None
        }
    };
    // `RepSegmentationConfig`'s default `min_velocity` (5.0) is tuned for
    // uncalibrated px/s data; m/s bar speeds are typically well under 1-2
    // m/s, so with a `Calibration` the dead-band must be overridden much
    // smaller (per `rep.rs`'s own doc comment) or every sample stays
    // `Idle` and zero reps are ever detected.
    let reps = velocity
        .as_ref()
        .map(|v| segment_run_reps(seed.lift, v, cal.is_some()))
        .unwrap_or_default();
    let metrics = velocity
        .as_ref()
        .map(|v| all_rep_metrics(&reps, v, bar_path.points(), cal.as_ref()))
        .unwrap_or_default();

    let csv_path = args.out_dir.join(format!("{stem}.csv"));
    let json_path = args.out_dir.join(format!("{stem}.json"));
    let reps_csv_path = args.out_dir.join(format!("{stem}.reps.csv"));
    let reps_json_path = args.out_dir.join(format!("{stem}.reps.json"));
    let overlay_path = args.out_dir.join(format!("{stem}.overlay.mp4"));

    std::fs::write(
        &csv_path,
        export_csv(&bar_path, cal.as_ref(), velocity.as_deref()),
    )
    .map_err(|e| format!("failed to write {}: {e}", csv_path.display()))?;
    std::fs::write(
        &json_path,
        export_json(&bar_path, cal.as_ref(), velocity.as_deref()),
    )
    .map_err(|e| format!("failed to write {}: {e}", json_path.display()))?;
    std::fs::write(&reps_csv_path, export_reps_csv(&metrics))
        .map_err(|e| format!("failed to write {}: {e}", reps_csv_path.display()))?;
    std::fs::write(&reps_json_path, export_reps_json(&metrics))
        .map_err(|e| format!("failed to write {}: {e}", reps_json_path.display()))?;
    if let Some(p) = write_run_sidecar(&args.out_dir, stem, &seed)? {
        println!("run info (UNVERIFIED) -> {}", p.display());
    }
    tracing::info!(
        csv_path = %csv_path.display(),
        json_path = %json_path.display(),
        reps_csv_path = %reps_csv_path.display(),
        reps_json_path = %reps_json_path.display(),
        overlay_path = %overlay_path.display(),
        "wrote track exports"
    );

    render_overlay_video(
        &args.video_path,
        &overlay_path,
        metadata.display_width(),
        metadata.display_height(),
        metadata.fps_num,
        metadata.fps_den,
        &bar_path,
        &reps,
    )?;

    let found = bar_path
        .points()
        .iter()
        .filter(|p| p.source == Source::Tracked)
        .count();
    let interpolated = bar_path.points().len() - found;
    println!(
        "{}: {} points ({found} tracked, {interpolated} interpolated), {} gap(s), {reseed_events} reseed event(s) needed -> {} / {} / {}",
        args.video_path.display(),
        bar_path.points().len(),
        bar_path.gaps().len(),
        csv_path.display(),
        json_path.display(),
        overlay_path.display(),
    );

    for (i, m) in metrics.iter().enumerate() {
        let unit = match m.unit {
            tracker_core::VelocityUnit::PixelsPerSecond => "px/s",
            tracker_core::VelocityUnit::MetersPerSecond => "m/s",
        };
        let depth_unit = if cal.is_some() { "m" } else { "px" };
        println!(
            "rep {i}: depth={:.3}{depth_unit} peak={:.3}{unit} mean={:.3}{unit} ({} interpolated sample(s) excluded)",
            m.depth, m.peak_concentric_speed, m.mean_concentric_velocity, m.excluded_interpolated_samples
        );
    }
    if metrics.is_empty() {
        if velocity.is_none() {
            println!("(no reps: velocity could not be computed -- see warning above)");
        } else {
            println!("(no reps detected)");
        }
    }
    println!(
        "reps -> {} / {}",
        reps_csv_path.display(),
        reps_json_path.display()
    );

    Ok(())
}

// --- `advise` subcommand (task 6.2) ---

/// Parsed `advise` subcommand arguments.
pub struct AdviseArgs {
    pub video_path: PathBuf,
    /// How many recommendations to print. Defaults to 3.
    pub top_n: usize,
}

/// Parses `advise <video> [--top-n N]` from the args following the
/// subcommand name itself.
pub fn parse_advise_args(args: &[String]) -> Result<AdviseArgs, CliError> {
    let mut video_path: Option<PathBuf> = None;
    let mut top_n: usize = 3;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top-n" => {
                let v = args.get(i + 1).ok_or("--top-n needs a value")?;
                top_n = v.parse().map_err(|_| format!("bad --top-n: {v}"))?;
                i += 2;
            }
            other if video_path.is_none() && !other.starts_with("--") => {
                video_path = Some(PathBuf::from(other));
                i += 1;
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(AdviseArgs {
        video_path: video_path.ok_or("missing <video> argument")?,
        top_n,
    })
}

/// Number of frames sampled across the video's duration for the hue
/// histogram (task 6.2): spread out rather than clustered near the start,
/// so a quick-cut intro/outro doesn't dominate the recommendation.
const ADVISE_SAMPLE_COUNT: u64 = 10;

/// Runs the `advise` subcommand (task 6.2, "Marker Color Advisor",
/// CONTEXT.md): probes the video, seeks to `ADVISE_SAMPLE_COUNT` frames
/// spread evenly across its duration, builds a hue histogram (6.1) over
/// them, and prints ranked marker-hue recommendations.
pub fn run_advise(args: AdviseArgs) -> Result<(), CliError> {
    let metadata = ffprobe::probe(&args.video_path)
        .map_err(|e| format!("failed to probe {}: {e}", args.video_path.display()))?;

    // ffprobe's `nb_frames` is absent for some containers; fall back to a
    // conservative guess (a few hundred frames) rather than failing the
    // whole command over a purely cosmetic sampling-spread concern.
    let frame_count = metadata.frame_count.unwrap_or(300).max(1);

    let mut decoder = SeekingFrameDecoder::new(
        args.video_path.clone(),
        metadata.display_width(),
        metadata.display_height(),
        metadata.fps_num,
        metadata.fps_den,
    );

    let mut frames = Vec::new();
    for i in 0..ADVISE_SAMPLE_COUNT {
        // Evenly spread sample indices across [0, frame_count), inclusive
        // of neither the very first partial second nor the very last
        // (which is sometimes truncated), by sampling the frame_count/N
        // midpoints rather than the endpoints.
        let index = (i * frame_count) / ADVISE_SAMPLE_COUNT;
        match decoder.decode_frame(index) {
            Ok(frame) => frames.push(frame),
            Err(e) => {
                tracing::warn!(video_frame_index = index, "advise: skipping frame: {e}");
            }
        }
    }

    if frames.is_empty() {
        return Err(format!(
            "could not decode any sample frames from {}",
            args.video_path.display()
        ));
    }

    let frame_refs: Vec<&tracker_core::Frame> = frames.iter().collect();
    let hist = hue_histogram(&frame_refs, HueHistogramConfig::default());
    let recommendations = recommend_marker_hues(&hist, args.top_n);

    println!(
        "{}: sampled {} frame(s), {} scene pixel(s) counted",
        args.video_path.display(),
        frames.len(),
        hist.total_counted()
    );
    if recommendations.is_empty() {
        println!("(no recommendations)");
    }
    for (rank, rec) in recommendations.iter().enumerate() {
        println!(
            "{}. {} ({:.0}\u{b0}) -- scene presence {:.1}%",
            rank + 1,
            rec.name,
            rec.hue_degrees,
            rec.scene_presence * 100.0
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_track_args() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "42",
            "--seed",
            "10.5,20.25",
            "--out",
            "out/dir",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(parsed.video_path, PathBuf::from("video.mp4"));
        assert_eq!(parsed.seed_frame, Some(42));
        assert_eq!(parsed.seed, Some(Point::new(10.5, 20.25)));
        assert_eq!(parsed.out_dir, PathBuf::from("out/dir"));
        assert_eq!(parsed.tuning.patch_radius, None);
        assert_eq!(parsed.tuning.search_radius, None);
        assert_eq!(parsed.tuning.min_score, None);
        assert_eq!(parsed.tuning.update_threshold, None);
        assert_eq!(parsed.tuning.coast_limit, None);
        assert_eq!(parsed.tracker_selection, tracking::TrackerSelection::Auto);
        assert_eq!(parsed.cal_points, None);
        assert_eq!(parsed.cal_length_m, None);
    }

    #[test]
    fn parses_cal_flags() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--cal",
            "200,120,320,120",
            "--cal-length-m",
            "0.45",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(
            parsed.cal_points,
            Some((Point::new(200.0, 120.0), Point::new(320.0, 120.0)))
        );
        assert_eq!(parsed.cal_length_m, Some(0.45));
    }

    #[test]
    fn bad_cal_format_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--cal",
            "200,120",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_track_args(&args).is_err());
    }

    #[test]
    fn parses_tracker_selection_flag() {
        for (flag, expected) in [
            ("auto", tracking::TrackerSelection::Auto),
            ("template", tracking::TrackerSelection::Template),
            ("color", tracking::TrackerSelection::Color),
        ] {
            let args: Vec<String> = vec![
                "video.mp4",
                "--seed-frame",
                "0",
                "--seed",
                "1,2",
                "--out",
                "out",
                "--tracker",
                flag,
            ]
            .into_iter()
            .map(String::from)
            .collect();
            let parsed = parse_track_args(&args).unwrap();
            assert_eq!(parsed.tracker_selection, expected);
        }
    }

    #[test]
    fn bad_tracker_selection_flag_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--tracker",
            "bogus",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_track_args(&args).is_err());
    }

    #[test]
    fn parses_optional_tuning_flags() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "42",
            "--seed",
            "10.5,20.25",
            "--out",
            "out/dir",
            "--patch-radius",
            "20",
            "--search-radius",
            "45",
            "--min-score",
            "0.55",
            "--update-threshold",
            "0.75",
            "--coast-limit",
            "8",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(parsed.tuning.patch_radius, Some(20));
        assert_eq!(parsed.tuning.search_radius, Some(45));
        assert_eq!(parsed.tuning.min_score, Some(0.55));
        assert_eq!(parsed.tuning.update_threshold, Some(0.75));
        assert_eq!(parsed.tuning.coast_limit, Some(8));
    }

    // --- Task 11.3: --filter ---

    #[test]
    fn parse_filter_spec_parses_gaussian() {
        let f = parse_filter_spec("gaussian:1.5").unwrap();
        assert_eq!(f, tracker_core::Preprocessor::GaussianBlur { sigma: 1.5 });
    }

    #[test]
    fn parse_filter_spec_parses_median() {
        let f = parse_filter_spec("median:3").unwrap();
        assert_eq!(f, tracker_core::Preprocessor::Median { k: 3 });
    }

    #[test]
    fn parse_filter_spec_rejects_unknown_kind() {
        let err = parse_filter_spec("sharpen:2").unwrap_err();
        assert!(err.contains("bad --filter"), "unexpected error: {err}");
    }

    #[test]
    fn parse_filter_spec_rejects_missing_colon() {
        let err = parse_filter_spec("gaussian1.5").unwrap_err();
        assert!(err.contains("bad --filter"), "unexpected error: {err}");
    }

    #[test]
    fn parse_filter_spec_rejects_non_numeric_param() {
        assert!(parse_filter_spec("gaussian:abc").is_err());
        assert!(parse_filter_spec("median:abc").is_err());
    }

    #[test]
    fn single_filter_flag_is_wired_into_tuning_chain() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--filter",
            "gaussian:1.5",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(
            parsed.tuning.preprocessor.steps(),
            &[tracker_core::Preprocessor::GaussianBlur { sigma: 1.5 }]
        );
    }

    #[test]
    fn repeated_filter_flags_preserve_order_in_the_chain() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--filter",
            "median:3",
            "--filter",
            "gaussian:1.5",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_track_args(&args).unwrap();
        assert_eq!(
            parsed.tuning.preprocessor.steps(),
            &[
                tracker_core::Preprocessor::Median { k: 3 },
                tracker_core::Preprocessor::GaussianBlur { sigma: 1.5 },
            ]
        );

        // Reversed flag order produces the reversed chain order.
        let args_rev: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--filter",
            "gaussian:1.5",
            "--filter",
            "median:3",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed_rev = parse_track_args(&args_rev).unwrap();
        assert_eq!(
            parsed_rev.tuning.preprocessor.steps(),
            &[
                tracker_core::Preprocessor::GaussianBlur { sigma: 1.5 },
                tracker_core::Preprocessor::Median { k: 3 },
            ]
        );
    }

    #[test]
    fn no_filter_flag_leaves_chain_empty() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_track_args(&args).unwrap();
        assert!(parsed.tuning.preprocessor.is_empty());
    }

    #[test]
    fn bad_filter_flag_value_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--filter",
            "bogus:1",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let err = parse_track_args(&args).unwrap_err();
        assert!(err.contains("bad --filter"), "unexpected error: {err}");
    }

    #[test]
    fn bad_tuning_flag_value_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "1,2",
            "--out",
            "out",
            "--min-score",
            "not-a-number",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_track_args(&args).is_err());
    }

    #[test]
    fn missing_required_flag_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--seed-frame", "42"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_track_args(&args).is_err());
    }

    #[test]
    fn bad_seed_format_is_an_error() {
        let args: Vec<String> = vec![
            "video.mp4",
            "--seed-frame",
            "0",
            "--seed",
            "not-a-point",
            "--out",
            "out",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_track_args(&args).is_err());
    }

    // --- parse_advise_args ---

    #[test]
    fn parses_advise_args_with_default_top_n() {
        let args: Vec<String> = vec!["video.mp4"].into_iter().map(String::from).collect();
        let parsed = parse_advise_args(&args).unwrap();
        assert_eq!(parsed.video_path, PathBuf::from("video.mp4"));
        assert_eq!(parsed.top_n, 3);
    }

    #[test]
    fn parses_advise_args_with_top_n_override() {
        let args: Vec<String> = vec!["video.mp4", "--top-n", "5"]
            .into_iter()
            .map(String::from)
            .collect();
        let parsed = parse_advise_args(&args).unwrap();
        assert_eq!(parsed.top_n, 5);
    }

    #[test]
    fn advise_missing_video_is_an_error() {
        let args: Vec<String> = vec!["--top-n", "5"].into_iter().map(String::from).collect();
        assert!(parse_advise_args(&args).is_err());
    }

    #[test]
    fn advise_bad_top_n_is_an_error() {
        let args: Vec<String> = vec!["video.mp4", "--top-n", "not-a-number"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(parse_advise_args(&args).is_err());
    }

    // --- auto mode (no seed) ---

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn track_args_without_seed_means_auto() {
        let a = parse_track_args(&sv(&["v.mp4", "--out", "o"])).unwrap();
        assert_eq!(a.seed_frame, None);
        assert_eq!(a.seed, None);
    }

    #[test]
    fn track_args_with_only_one_of_seed_and_seed_frame_is_a_usage_error() {
        let e = parse_track_args(&sv(&["v.mp4", "--seed-frame", "3", "--out", "o"])).unwrap_err();
        assert!(e.contains("--seed-frame") && e.contains("--seed"), "{e}");
        let e = parse_track_args(&sv(&["v.mp4", "--seed", "1,2", "--out", "o"])).unwrap_err();
        assert!(e.contains("--seed-frame") && e.contains("--seed"), "{e}");
    }

    #[test]
    fn track_args_with_seed_is_unchanged() {
        let a = parse_track_args(&sv(&[
            "v.mp4",
            "--seed-frame",
            "3",
            "--seed",
            "1.5,2",
            "--out",
            "o",
        ]))
        .unwrap();
        assert_eq!(
            (a.seed_frame, a.seed),
            (Some(3), Some(Point::new(1.5, 2.0)))
        );
    }

    struct NoPlate;
    impl FrameDecoder for NoPlate {
        type Error = String;
        type Frame = tracker_core::Frame;
        fn decode_frame(&mut self, _i: u64) -> Result<Self::Frame, String> {
            Ok(tracker_core::Frame::new(120, 160, vec![90; 120 * 160 * 3]).unwrap())
        }
    }

    #[test]
    fn auto_run_with_no_proposal_returns_clear_error_not_panic() {
        let err = resolve_seed(None, &mut NoPlate, 40, |_, _| {}).unwrap_err();
        assert!(
            err.contains("no bar") && err.contains("--seed-frame"),
            "{err}"
        );
    }

    #[test]
    fn manual_seed_resolves_without_touching_the_decoder() {
        struct Panics;
        impl FrameDecoder for Panics {
            type Error = String;
            type Frame = tracker_core::Frame;
            fn decode_frame(&mut self, _i: u64) -> Result<Self::Frame, String> {
                panic!("manual seed must not run the pre-pass");
            }
        }
        let r = resolve_seed(Some((7, Point::new(1.0, 2.0))), &mut Panics, 40, |_, _| {}).unwrap();
        assert_eq!((r.frame_index, r.position), (7, Point::new(1.0, 2.0)));
        assert!(r.auto.is_none());
        assert_eq!(r.lift, tracker_core::LiftType::Unknown);
    }

    #[test]
    fn manual_run_writes_no_sidecar() {
        let dir = std::env::temp_dir().join(format!("cli-nosidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = ResolvedSeed {
            frame_index: 0,
            position: Point::new(0.0, 0.0),
            lift: tracker_core::LiftType::Unknown,
            auto: None,
        };
        assert_eq!(write_run_sidecar(&dir, "clip", &r).unwrap(), None);
        assert!(!dir.join("clip.run.json").exists());
    }

    fn manual_seed() -> ResolvedSeed {
        ResolvedSeed {
            frame_index: 0,
            position: Point::new(0.0, 0.0),
            lift: tracker_core::LiftType::Unknown,
            auto: None,
        }
    }

    fn auto_seed() -> ResolvedSeed {
        ResolvedSeed {
            frame_index: 12,
            position: Point::new(3.0, 4.0),
            lift: tracker_core::LiftType::Deadlift,
            auto: Some(AutoSeedInfo {
                lift_reason: "rises from floor".into(),
                plate_px: 88.0,
                confidence: 0.9,
            }),
        }
    }

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn manual_run_removes_a_stale_sidecar_for_its_stem() {
        let dir = fresh_dir("stale");
        write_run_sidecar(&dir, "clip", &auto_seed()).unwrap();
        assert!(dir.join("clip.run.json").exists());
        assert_eq!(
            write_run_sidecar(&dir, "clip", &manual_seed()).unwrap(),
            None
        );
        assert!(!dir.join("clip.run.json").exists());
    }

    #[test]
    fn manual_run_leaves_other_stems_sidecars_alone() {
        let dir = fresh_dir("others");
        write_run_sidecar(&dir, "other", &auto_seed()).unwrap();
        write_run_sidecar(&dir, "clip", &manual_seed()).unwrap();
        assert!(dir.join("other.run.json").exists());
    }

    #[test]
    fn manual_run_with_no_sidecar_present_is_fine() {
        let dir = fresh_dir("none");
        assert_eq!(
            write_run_sidecar(&dir, "clip", &manual_seed()).unwrap(),
            None
        );
    }

    #[test]
    fn auto_run_overwrites_an_existing_sidecar() {
        let dir = fresh_dir("overwrite");
        std::fs::write(dir.join("clip.run.json"), "stale").unwrap();
        let p = write_run_sidecar(&dir, "clip", &auto_seed())
            .unwrap()
            .unwrap();
        let info = crate::run_info::read_run_info(&p).unwrap().unwrap();
        assert_eq!(info.seed_frame, 12);
    }

    #[test]
    fn auto_run_writes_unverified_sidecar() {
        let dir = std::env::temp_dir().join(format!("cli-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = ResolvedSeed {
            frame_index: 12,
            position: Point::new(3.0, 4.0),
            lift: tracker_core::LiftType::Deadlift,
            auto: Some(AutoSeedInfo {
                lift_reason: "rises from floor".into(),
                plate_px: 88.0,
                confidence: 0.9,
            }),
        };
        let p = write_run_sidecar(&dir, "clip", &r).unwrap().unwrap();
        let info = crate::run_info::read_run_info(&p).unwrap().unwrap();
        assert_eq!(info.lift, "deadlift");
        assert!(!info.verified);
        assert_eq!(info.seed_frame, 12);
        assert_eq!(info.plate_px, 88.0);
        assert_eq!(info.seed, [3.0, 4.0]);
        assert_eq!(info.lift_reason, "rises from floor");
    }

    #[test]
    fn deadlift_lift_threads_concentric_first_reps_through_segmentation() {
        use tracker_core::{LiftType, PhaseOrder, VelocitySample, VelocityUnit};
        let mut vys = vec![0.0; 10];
        vys.extend(vec![-200.0; 30]); // pull
        vys.extend(vec![200.0; 30]); // lower
        vys.extend(vec![0.0; 10]);
        let velocity: Vec<VelocitySample> = vys
            .iter()
            .enumerate()
            .map(|(i, &vy)| VelocitySample {
                frame_index: i as u64,
                t_seconds: i as f64 / 30.0,
                vx: 0.0,
                vy,
                speed: vy.abs(),
                unit: VelocityUnit::PixelsPerSecond,
                from_interpolated: false,
            })
            .collect();
        let dl = segment_run_reps(LiftType::Deadlift, &velocity, false);
        assert_eq!(dl.len(), 1);
        assert_eq!(dl[0].order, PhaseOrder::ConcentricFirst);
        assert_eq!(dl[0].bottom_index(), 10);
        assert_eq!(*dl[0].concentric_span().start(), 10);
        // Unknown keeps today's eccentric-first behaviour.
        for r in segment_run_reps(LiftType::Unknown, &velocity, false) {
            assert_eq!(r.order, PhaseOrder::EccentricFirst);
        }
    }
}
