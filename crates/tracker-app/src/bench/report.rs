use crate::tracking::TrackerSelection;

use super::strategy::{recommend_viable, BenchmarkRow};

/// Score unit label for a strategy's tracker kind, used by both the table
/// and the JSON export so a reader never mistakes a color fill-fraction for
/// a template correlation score.
fn score_unit(tracker: TrackerSelection) -> &'static str {
    match tracker {
        TrackerSelection::Template => "correlation",
        TrackerSelection::Color => "fill-fraction",
        TrackerSelection::Circle => "edge-support",
        TrackerSelection::Auto => "?",
    }
}

/// Renders an aligned stdout table plus a recommendation line, given the
/// already-computed rows. (20.3) Gated rows (`gate_reason`) are marked
/// `[GATED]` in the strategy column and can never win the recommendation
/// (`recommend_viable`) -- Color/Circle rows stay visible for comparison,
/// but never read as an endorsed choice.
pub fn format_table(rows: &[BenchmarkRow]) -> String {
    let winner = recommend_viable(rows);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<18} {:>10} {:>7} {:>5} {:>8} {:>12} {:>10}\n",
        "strategy", "tracked%", "misses", "gaps", "reseeds", "mean score", "jitter(px)"
    ));
    for (i, row) in rows.iter().enumerate() {
        let m = &row.metrics;
        let score = match m.mean_score {
            Some(s) => format!("{:.3} ({})", s, score_unit(row.strategy.tracker)),
            None => "-".to_string(),
        };
        let jitter = m
            .mean_jitter
            .map(|j| format!("{j:.2}"))
            .unwrap_or_else(|| "-".to_string());
        let marker = if winner == Some(i) { " *" } else { "" };
        let label = if row.gated_reason.is_some() {
            format!("[GATED] {}", row.strategy.label())
        } else {
            row.strategy.label()
        };
        out.push_str(&format!(
            "{:<18} {:>9.1}% {:>7} {:>5} {:>8} {:>16} {:>10}{marker}\n",
            label, m.tracked_pct, m.misses, m.gaps, m.reseeds, score, jitter
        ));
        if let Some(reason) = &row.gated_reason {
            out.push_str(&format!("  gated: {reason}\n"));
        }
        if let Some(note) = &row.note {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    if let Some(w) = winner {
        out.push_str(&format!(
            "\nrecommendation: {} (highest tracked%, tie-break: lowest jitter, gated \
             strategies excluded)\n",
            rows[w].strategy.label()
        ));
    } else if !rows.is_empty() {
        out.push_str("\nrecommendation: none (every strategy is gated)\n");
    }
    out
}

/// Machine-readable JSON export of the benchmark (11.4's `--out`).
pub fn to_json(rows: &[BenchmarkRow]) -> String {
    let winner = recommend_viable(rows);

    let entries: Vec<String> = rows
        .iter()
        .map(|row| {
            let m = &row.metrics;
            format!(
                "{{\"strategy\":\"{}\",\"filter\":\"{}\",\"tracker\":\"{}\",\
                 \"frames_total\":{},\"tracked_pct\":{},\"misses\":{},\"gaps\":{},\
                 \"reseeds\":{},\"mean_score\":{},\"score_unit\":\"{}\",\"mean_jitter_px\":{},\"note\":{},\
                 \"gated\":{},\"gated_reason\":{}}}",
                row.strategy.label(),
                row.strategy.filter.label(),
                score_unit(row.strategy.tracker),
                m.frames_total,
                m.tracked_pct,
                m.misses,
                m.gaps,
                m.reseeds,
                m.mean_score
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                score_unit(row.strategy.tracker),
                m.mean_jitter
                    .map(|j| j.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                row.note
                    .as_ref()
                    .map(|n| format!("\"{}\"", n.replace('"', "'")))
                    .unwrap_or_else(|| "null".to_string()),
                row.gated_reason.is_some(),
                row.gated_reason
                    .as_ref()
                    .map(|n| format!("\"{}\"", n.replace('"', "'")))
                    .unwrap_or_else(|| "null".to_string()),
            )
        })
        .collect();

    format!(
        "{{\"strategies\":[{}],\"recommendation\":{}}}",
        entries.join(","),
        winner
            .map(|w| format!("\"{}\"", rows[w].strategy.label()))
            .unwrap_or_else(|| "null".to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::strategy::tests::{gated_row, row};
    use crate::bench::strategy::{FilterKind, Strategy};

    #[test]
    fn format_table_marks_the_recommended_row() {
        let rows = vec![
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Template,
                },
                80.0,
                Some(2.0),
            ),
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Color,
                },
                95.0,
                Some(1.0),
            ),
        ];
        let table = format_table(&rows);
        assert!(table.contains("recommendation: none/color"));
    }

    #[test]
    fn to_json_includes_every_strategy_and_the_recommendation() {
        let rows = vec![
            row(
                Strategy {
                    filter: FilterKind::None,
                    tracker: TrackerSelection::Template,
                },
                80.0,
                Some(2.0),
            ),
            row(
                Strategy {
                    filter: FilterKind::Median3,
                    tracker: TrackerSelection::Color,
                },
                95.0,
                Some(1.0),
            ),
        ];
        let json = to_json(&rows);
        assert!(json.contains("\"none/template\""));
        assert!(json.contains("\"median:3/color\""));
        assert!(json.contains("\"recommendation\":\"median:3/color\""));
    }

    #[test]
    fn format_table_marks_a_gated_row_and_never_recommends_it() {
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
        let table = format_table(&rows);
        assert!(table.contains("[GATED] none/color"));
        assert!(table.contains("gated: indistinct color"));
        assert!(table.contains("recommendation: none/template"));
        assert!(!table.contains("recommendation: none/color"));
    }

    #[test]
    fn format_table_says_no_recommendation_when_every_row_is_gated() {
        let rows = vec![gated_row(
            Strategy {
                filter: FilterKind::None,
                tracker: TrackerSelection::Circle,
            },
            100.0,
            Some(0.0),
            "documented negative result",
        )];
        let table = format_table(&rows);
        assert!(table.contains("recommendation: none (every strategy is gated)"));
    }

    #[test]
    fn to_json_marks_gated_rows_and_excludes_them_from_recommendation() {
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
        let json = to_json(&rows);
        assert!(json.contains("\"gated\":false"));
        assert!(json.contains("\"gated\":true"));
        assert!(json.contains("\"gated_reason\":\"indistinct color\""));
        assert!(json.contains("\"recommendation\":\"none/template\""));
    }
}
