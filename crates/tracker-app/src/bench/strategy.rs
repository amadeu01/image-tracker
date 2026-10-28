use tracker_core::{Point, Preprocessor, PreprocessorChain, TrackerKind};

use crate::tracking::TrackerSelection;

/// The filter-chain half of the strategy matrix (11.4): a fixed three
/// options, matching the ones already reachable via `--filter` (11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    None,
    Gaussian1_5,
    Median3,
}

impl FilterKind {
    /// All three filter options, in the fixed order the matrix/table use.
    pub const ALL: [FilterKind; 3] = [
        FilterKind::None,
        FilterKind::Gaussian1_5,
        FilterKind::Median3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterKind::None => "none",
            FilterKind::Gaussian1_5 => "gaussian:1.5",
            FilterKind::Median3 => "median:3",
        }
    }

    pub fn chain(self) -> PreprocessorChain {
        match self {
            FilterKind::None => PreprocessorChain::new(),
            FilterKind::Gaussian1_5 => {
                PreprocessorChain::from_steps(vec![Preprocessor::GaussianBlur { sigma: 1.5 }])
            }
            FilterKind::Median3 => {
                PreprocessorChain::from_steps(vec![Preprocessor::Median { k: 3 }])
            }
        }
    }
}

/// One cell of the strategy matrix: a filter chain paired with a forced
/// tracker kind (never `Auto` -- the whole point is comparing both kinds
/// explicitly, even where `suggest_tracker` would pick the other one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strategy {
    pub filter: FilterKind,
    pub tracker: TrackerSelection,
}

impl Strategy {
    pub fn label(&self) -> String {
        let tracker = match self.tracker {
            TrackerSelection::Template => "template",
            TrackerSelection::Color => "color",
            TrackerSelection::Circle => "circle",
            TrackerSelection::Auto => "auto", // never actually produced by strategy_matrix
        };
        format!("{}/{tracker}", self.filter.label())
    }

    /// Filename-safe slug for this strategy (14.1), used in overlay video
    /// names: `<stem>.<slug>.overlay.mp4`. Derived from the same matrix
    /// labels as `label()` but with no `/`, `:`, or spaces — e.g.
    /// `gaussian1.5-template`, `median3-color`, `none-template`. Unique
    /// across `strategy_matrix()` because both halves are.
    pub fn slug(&self) -> String {
        let filter = match self.filter {
            FilterKind::None => "none",
            FilterKind::Gaussian1_5 => "gaussian1.5",
            FilterKind::Median3 => "median3",
        };
        let tracker = match self.tracker {
            TrackerSelection::Template => "template",
            TrackerSelection::Color => "color",
            TrackerSelection::Circle => "circle",
            TrackerSelection::Auto => "auto", // never produced by strategy_matrix
        };
        format!("{filter}-{tracker}")
    }
}

/// The fixed 3x2+1 strategy matrix (11.4, +17.5): {none, gaussian:1.5,
/// median:3} x {template, color} (6 entries), plus a 7th entry for the
/// plate-circle tracker (17.5, audit F6) — geometric circle fit rather than
/// appearance matching, so it doesn't vary by filter the same way (edge
/// detection has its own smoothing built in); `none` is the one filter
/// entry it's benchmarked under. Stable order so table rows/JSON entries/the
/// `recommend` tie-break are all reproducible run to run.
pub fn strategy_matrix() -> Vec<Strategy> {
    let mut out = Vec::with_capacity(7);
    for filter in FilterKind::ALL {
        out.push(Strategy {
            filter,
            tracker: TrackerSelection::Template,
        });
        out.push(Strategy {
            filter,
            tracker: TrackerSelection::Color,
        });
    }
    out.push(Strategy {
        filter: FilterKind::None,
        tracker: TrackerSelection::Circle,
    });
    out
}

/// One frame's `Tracker::step` result, kept in the shape `compute_metrics`
/// needs (a plain enum over position+score, decoupled from
/// `tracker_core::StepOutcome` only so this module's pure functions don't
/// need a live `Frame`/`Tracker` to unit test against).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameOutcome {
    Found { position: Point, score: f64 },
    Miss,
}

/// Computed metrics for one strategy's run (11.4). `mean_score`/`mean_jitter`
/// are `None` when there weren't enough tracked samples to average (zero
/// tracked frames, or exactly one for jitter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyMetrics {
    pub frames_total: u64,
    pub tracked_pct: f64,
    pub misses: u64,
    pub gaps: u64,
    pub reseeds: u64,
    pub mean_score: Option<f64>,
    pub mean_jitter: Option<f64>,
}

/// Reduces a strategy's per-frame outcomes to `StrategyMetrics`. Pure and
/// synthesizable directly (no decode/tracker needed), so this is the core of
/// 11.4's TDD surface.
///
/// - `gaps`: the number of separate miss-streaks (a transition from a
///   tracked/start frame into `Miss`), not the raw miss count.
/// - `reseeds`: how many times a miss-streak would exceed `coast_limit` and
///   force an auto-reseed (mirroring the CLI `track` subcommand's headless
///   auto-resume, `cli::run_track`) -- the streak counter resets to zero
///   each time this fires, so a very long loss is counted as a fresh gap
///   each time it triggers a reseed (a reseed conceptually closes the
///   current gap; any further misses open a new one).
/// - `mean_score`: averaged only over `Found` outcomes; callers decide the
///   unit label (correlation for template, fill-fraction for color) --
///   this function doesn't know which tracker produced the outcomes.
/// - `mean_jitter`: mean Euclidean distance between *consecutive tracked*
///   samples (skipping over any intervening misses), not consecutive
///   frames.
pub fn compute_metrics(outcomes: &[FrameOutcome], coast_limit: u32) -> StrategyMetrics {
    let total = outcomes.len() as u64;
    let mut tracked = 0u64;
    let mut misses = 0u64;
    let mut gaps = 0u64;
    let mut reseeds = 0u64;
    let mut consecutive_miss: u32 = 0;
    let mut score_sum = 0.0;
    let mut score_count = 0u64;
    let mut last_tracked: Option<Point> = None;
    let mut jitter_sum = 0.0;
    let mut jitter_count = 0u64;

    for outcome in outcomes {
        match *outcome {
            FrameOutcome::Found { position, score } => {
                tracked += 1;
                score_sum += score;
                score_count += 1;
                if let Some(prev) = last_tracked {
                    let dx = position.x - prev.x;
                    let dy = position.y - prev.y;
                    jitter_sum += (dx * dx + dy * dy).sqrt();
                    jitter_count += 1;
                }
                last_tracked = Some(position);
                consecutive_miss = 0;
            }
            FrameOutcome::Miss => {
                misses += 1;
                if consecutive_miss == 0 {
                    gaps += 1;
                }
                consecutive_miss += 1;
                if consecutive_miss > coast_limit {
                    reseeds += 1;
                    consecutive_miss = 0;
                }
            }
        }
    }

    StrategyMetrics {
        frames_total: total,
        tracked_pct: if total == 0 {
            0.0
        } else {
            tracked as f64 / total as f64 * 100.0
        },
        misses,
        gaps,
        reseeds,
        mean_score: (score_count > 0).then_some(score_sum / score_count as f64),
        mean_jitter: (jitter_count > 0).then_some(jitter_sum / jitter_count as f64),
    }
}

/// Picks the recommended strategy's index: highest `tracked_pct` wins; on an
/// exact tie, lowest `mean_jitter` wins (a strategy with no tracked samples
/// at all -- `mean_jitter: None` -- sorts as worst-possible jitter, i.e.
/// loses every tie it's part of); if *that's* also tied (including both
/// `None`), the earlier strategy in `strategy_matrix`'s fixed order wins,
/// since `best` is only replaced on a strict improvement below. Returns
/// `None` for an empty slice.
pub fn recommend(results: &[StrategyMetrics]) -> Option<usize> {
    if results.is_empty() {
        return None;
    }
    let mut best = 0usize;
    for (i, cand) in results.iter().enumerate().skip(1) {
        let best_m = &results[best];
        let better = cand.tracked_pct > best_m.tracked_pct
            || (cand.tracked_pct == best_m.tracked_pct
                && cand.mean_jitter.unwrap_or(f64::INFINITY)
                    < best_m.mean_jitter.unwrap_or(f64::INFINITY));
        if better {
            best = i;
        }
    }
    Some(best)
}

/// (20.3, promoting 17.6) Whether `tracker` is presented as a viable
/// candidate in the `compare` benchmark's table/JSON/recommendation, and why
/// not when it isn't. Both disproven trackers from milestone 17 stay in
/// `strategy_matrix()` -- `compare`'s whole purpose is comparing strategies
/// explicitly, including ones a user has opted into via `--tracker`/the
/// settings dropdown (`TrackerSelection`) -- but neither may look like a
/// viable default: this is presentation gating, not deletion or removal of
/// capability.
///
/// - `Circle` is always gated: a documented negative result (PLAN 17.5),
///   17%/0% accuracy vs Template's 85%/100% on the graded videos. No signal
///   in the seed/frame makes it situationally trustworthy the way Color's
///   colour-distinctness does, so unlike Color there's no conditional case
///   to check -- it's gated unconditionally.
/// - `Color` is gated exactly when `suggestion` (the seed patch's
///   `suggest_tracker` result, already computed by the caller) is not
///   `TrackerKind::Color` -- i.e. the same "is this seed patch a real,
///   colour-distinct Marker" signal `TrackerSelection::Auto` already defers
///   to in `tracking.rs`. Reusing it here (rather than inventing a new
///   threshold) is deliberate: milestone 17 framed itself as "no
///   threshold-tuning", and `suggest_tracker`'s
///   saturation/background-match thresholds already encode exactly the
///   "distinct colour / real Marker" condition PLAN 20.3 asks for.
/// - `Template`/`Auto` are never gated.
pub fn gate_reason(tracker: TrackerSelection, suggestion: Option<TrackerKind>) -> Option<String> {
    match tracker {
        TrackerSelection::Circle => Some(
            "Circle is a documented negative result (PLAN 17.5): 17%/0% accuracy vs Template's \
             85%/100% on the graded test videos. Kept for its research value, not recommended as \
             a default."
                .to_string(),
        ),
        TrackerSelection::Color if suggestion != Some(TrackerKind::Color) => Some(
            "seed color is not distinct from the background per suggest_tracker (would \
             recommend Template instead); Color is not recommended for this seed."
                .to_string(),
        ),
        _ => None,
    }
}

/// (20.3) Like `recommend`, but never picks a gated row (`gate_reason`):
/// disproven strategies must never be recommended as if they were viable,
/// no matter how good their raw numbers look (that's precisely Color's
/// deceptively-smooth-jitter failure mode this task exists to close).
/// Ties among the remaining viable rows still break the same way `recommend`
/// does. Returns `None` when every row is gated (nothing viable to
/// recommend), not the best of the gated rows.
pub fn recommend_viable(rows: &[BenchmarkRow]) -> Option<usize> {
    let viable_indices: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.gated_reason.is_none())
        .map(|(i, _)| i)
        .collect();
    if viable_indices.is_empty() {
        return None;
    }
    let viable_metrics: Vec<StrategyMetrics> =
        viable_indices.iter().map(|&i| rows[i].metrics).collect();
    recommend(&viable_metrics).map(|local| viable_indices[local])
}

/// One row of the finished benchmark: a strategy, its metrics, any note, and
/// (20.3) whether it's gated out of the "viable candidate" set and why.
#[derive(Debug, Clone)]
pub struct BenchmarkRow {
    pub strategy: Strategy,
    pub metrics: StrategyMetrics,
    pub note: Option<String>,
    pub gated_reason: Option<String>,
}

#[cfg(test)]
pub(in crate::bench) mod tests {
    use super::*;

    // --- strategy_matrix ---

    #[test]
    fn strategy_matrix_has_seven_entries_in_fixed_order() {
        let matrix = strategy_matrix();
        assert_eq!(matrix.len(), 7);
        let expected = [
            (FilterKind::None, TrackerSelection::Template),
            (FilterKind::None, TrackerSelection::Color),
            (FilterKind::Gaussian1_5, TrackerSelection::Template),
            (FilterKind::Gaussian1_5, TrackerSelection::Color),
            (FilterKind::Median3, TrackerSelection::Template),
            (FilterKind::Median3, TrackerSelection::Color),
            (FilterKind::None, TrackerSelection::Circle),
        ];
        for (strategy, (filter, tracker)) in matrix.iter().zip(expected.iter()) {
            assert_eq!(strategy.filter, *filter);
            assert_eq!(strategy.tracker, *tracker);
        }
    }

    #[test]
    fn strategy_label_includes_filter_and_tracker() {
        let s = Strategy {
            filter: FilterKind::Gaussian1_5,
            tracker: TrackerSelection::Color,
        };
        assert_eq!(s.label(), "gaussian:1.5/color");
    }

    #[test]
    fn filter_kind_chain_matches_expected_preprocessor_steps() {
        assert!(FilterKind::None.chain().is_empty());
        assert_eq!(
            FilterKind::Gaussian1_5.chain().steps(),
            &[Preprocessor::GaussianBlur { sigma: 1.5 }]
        );
        assert_eq!(
            FilterKind::Median3.chain().steps(),
            &[Preprocessor::Median { k: 3 }]
        );
    }

    // --- compute_metrics ---

    pub(in crate::bench) fn found(x: f64, y: f64, score: f64) -> FrameOutcome {
        FrameOutcome::Found {
            position: Point::new(x, y),
            score,
        }
    }

    #[test]
    fn compute_metrics_all_found_has_full_tracked_pct_and_no_gaps() {
        let outcomes = vec![
            found(0.0, 0.0, 0.9),
            found(1.0, 0.0, 0.9),
            found(2.0, 0.0, 0.9),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.frames_total, 3);
        assert_eq!(m.tracked_pct, 100.0);
        assert_eq!(m.misses, 0);
        assert_eq!(m.gaps, 0);
        assert_eq!(m.reseeds, 0);
        assert_eq!(m.mean_score, Some(0.9));
    }

    #[test]
    fn compute_metrics_empty_outcomes_is_all_zero_none() {
        let m = compute_metrics(&[], 5);
        assert_eq!(m.frames_total, 0);
        assert_eq!(m.tracked_pct, 0.0);
        assert_eq!(m.mean_score, None);
        assert_eq!(m.mean_jitter, None);
    }

    #[test]
    fn compute_metrics_counts_gaps_as_streaks_not_raw_misses() {
        // found, miss, miss, found, miss, found -- two separate miss streaks.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            found(1.0, 0.0, 0.8),
            FrameOutcome::Miss,
            found(2.0, 0.0, 0.8),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.misses, 3);
        assert_eq!(m.gaps, 2);
        assert_eq!(m.reseeds, 0); // never exceeds coast_limit=5 in one streak
    }

    #[test]
    fn compute_metrics_reseeds_once_a_streak_exceeds_coast_limit() {
        // 5 consecutive misses with coast_limit 3: reseed fires once the
        // streak passes 3 (on the 4th miss), then the counter resets so a
        // 5th miss starts a fresh (non-reseeding) streak.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
            FrameOutcome::Miss,
        ];
        let m = compute_metrics(&outcomes, 3);
        assert_eq!(m.misses, 5);
        // The streak resets on the auto-reseed (4th miss), so the 5th miss
        // starts a fresh streak/gap of its own: 2 gaps, 1 reseed.
        assert_eq!(m.gaps, 2);
        assert_eq!(m.reseeds, 1);
    }

    #[test]
    fn compute_metrics_jitter_is_mean_distance_between_consecutive_tracked_samples() {
        // Tracked at (0,0), (3,4) [dist 5], (3,4) [dist 0] -- skipping a Miss
        // in between the last two, which must not count as its own step.
        let outcomes = vec![
            found(0.0, 0.0, 0.8),
            found(3.0, 4.0, 0.8),
            FrameOutcome::Miss,
            found(3.0, 4.0, 0.8),
        ];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.mean_jitter, Some((5.0 + 0.0) / 2.0));
    }

    #[test]
    fn compute_metrics_jitter_is_none_with_fewer_than_two_tracked_samples() {
        let outcomes = vec![found(0.0, 0.0, 0.8), FrameOutcome::Miss];
        let m = compute_metrics(&outcomes, 5);
        assert_eq!(m.mean_jitter, None);
    }

    // --- recommend ---

    fn metrics(tracked_pct: f64, jitter: Option<f64>) -> StrategyMetrics {
        StrategyMetrics {
            frames_total: 100,
            tracked_pct,
            misses: 0,
            gaps: 0,
            reseeds: 0,
            mean_score: None,
            mean_jitter: jitter,
        }
    }

    #[test]
    fn recommend_picks_highest_tracked_pct() {
        let results = vec![
            metrics(80.0, Some(1.0)),
            metrics(95.0, Some(5.0)),
            metrics(50.0, Some(0.1)),
        ];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_tie_break_is_lowest_jitter() {
        let results = vec![
            metrics(90.0, Some(5.0)),
            metrics(90.0, Some(1.0)),
            metrics(90.0, Some(3.0)),
        ];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_full_tie_keeps_earliest_matrix_entry() {
        let results = vec![metrics(90.0, Some(2.0)), metrics(90.0, Some(2.0))];
        assert_eq!(recommend(&results), Some(0));
    }

    #[test]
    fn recommend_none_jitter_loses_ties_to_any_real_jitter() {
        let results = vec![metrics(90.0, None), metrics(90.0, Some(999.0))];
        assert_eq!(recommend(&results), Some(1));
    }

    #[test]
    fn recommend_empty_is_none() {
        assert_eq!(recommend(&[]), None);
    }

    // --- gate_reason / recommend_viable (20.3) ---

    #[test]
    fn circle_is_always_gated_regardless_of_suggestion() {
        assert!(gate_reason(TrackerSelection::Circle, Some(TrackerKind::Color)).is_some());
        assert!(gate_reason(TrackerSelection::Circle, None).is_some());
    }

    #[test]
    fn color_is_not_gated_when_suggest_tracker_recommends_color() {
        assert_eq!(
            gate_reason(TrackerSelection::Color, Some(TrackerKind::Color)),
            None
        );
    }

    #[test]
    fn color_is_gated_when_suggest_tracker_does_not_recommend_color() {
        assert!(gate_reason(TrackerSelection::Color, Some(TrackerKind::Template)).is_some());
        assert!(gate_reason(TrackerSelection::Color, None).is_some());
    }

    #[test]
    fn template_and_auto_are_never_gated() {
        assert_eq!(gate_reason(TrackerSelection::Template, None), None);
        assert_eq!(gate_reason(TrackerSelection::Auto, None), None);
    }

    #[test]
    fn recommend_viable_skips_a_gated_strategy_even_with_the_best_raw_numbers() {
        let rows = vec![
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Template,
                },
                80.0,
                Some(2.0),
            ),
            gated_row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Color,
                },
                99.0,
                Some(0.1),
                "indistinct color",
            ),
        ];
        assert_eq!(recommend_viable(&rows), Some(0));
    }

    #[test]
    fn recommend_viable_is_none_when_every_row_is_gated() {
        let rows = vec![gated_row(
            Strategy {
                filter: FilterKind::None,
                tracker: TrackerSelection::Circle,
            },
            100.0,
            Some(0.0),
            "documented negative result",
        )];
        assert_eq!(recommend_viable(&rows), None);
    }

    pub(in crate::bench) fn row(
        strategy: Strategy,
        tracked_pct: f64,
        jitter: Option<f64>,
    ) -> BenchmarkRow {
        BenchmarkRow {
            strategy,
            metrics: metrics(tracked_pct, jitter),
            note: None,
            gated_reason: None,
        }
    }

    pub(in crate::bench) fn gated_row(
        strategy: Strategy,
        tracked_pct: f64,
        jitter: Option<f64>,
        reason: &str,
    ) -> BenchmarkRow {
        BenchmarkRow {
            strategy,
            metrics: metrics(tracked_pct, jitter),
            note: None,
            gated_reason: Some(reason.to_string()),
        }
    }

    // --- slugs (14.1) ---

    #[test]
    fn strategy_slugs_are_filename_safe_and_unique_across_the_matrix() {
        let matrix = strategy_matrix();
        let slugs: Vec<String> = matrix.iter().map(|s| s.slug()).collect();
        for slug in &slugs {
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'),
                "slug not filename-safe: {slug}"
            );
            assert!(!slug.contains(':'), "slug contains colon: {slug}");
            assert!(!slug.contains(' '), "slug contains space: {slug}");
        }
        let mut deduped = slugs.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(deduped.len(), matrix.len(), "slugs not unique: {slugs:?}");
    }

    #[test]
    fn strategy_slug_matches_expected_shape() {
        let expected = [
            "none-template",
            "none-color",
            "gaussian1.5-template",
            "gaussian1.5-color",
            "median3-template",
            "median3-color",
            "none-circle",
        ];
        for (strategy, want) in strategy_matrix().iter().zip(expected.iter()) {
            assert_eq!(strategy.slug(), *want);
        }
    }
}
