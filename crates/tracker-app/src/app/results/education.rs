//! Metrics education copy for the Results section (task 19.4, split out of
//! `side_panel.rs` in 20.1), mirroring 14.2's `settings_section::education`
//! pattern: every user-facing explanation lives here as a named const, so
//! its prose is directly unit-testable rather than inline string literals
//! scattered through the render code. Grounded in docs/theory.md §9 (the
//! VBT background) — `TIP_VEL_LOSS` and `EXPLAIN_VELOCITY_LOSS` paraphrase
//! §9.1's velocity-loss definition and §9.3's evidence review, not invented
//! numbers. §9.3's own caveat — "20% is a defensible default, not a truth"
//! — is why the interpretation hint is worded as a coaching cue ("often
//! used to…"), never a verdict.

// -- Rep-table column header tooltips -------------------------------
pub const TIP_DEPTH: &str = "Vertical travel of the eccentric (descent) phase — how deep the \
     rep went, in px (or m once calibrated).";
pub const TIP_PEAK_V: &str = "The fastest instantaneous bar speed during the concentric (lift) \
     phase, in px/s (or m/s once calibrated). Noisier than Mean V — a \
     single fast sample can spike it.";
pub const TIP_MEAN_V: &str = "Mean concentric velocity (MV): concentric displacement ÷ duration. \
     The velocity-based-training literature's recommended default for \
     load profiling (theory.md §9.2) — steadier than Peak V.";
pub const TIP_LOSS: &str = "Velocity loss: this rep's Mean V vs rep 1's, as a percentage drop. \
     Rep 1 shows \"—\" (nothing to compare it to yet).";
pub const TIP_TIME: &str = "This rep's eccentric-start to concentric-end time range, \
     formatted M:SS.s from the video's frame rate.";

// -- Headline-card tooltips ------------------------------------------
pub const TIP_REPS: &str = "Number of reps this run detected (see the rep table below for \
     each one's depth/velocity/loss).";
pub const TIP_SET_TIME: &str = "Wall-clock duration of the set, from the first rep's eccentric \
     start to the last rep's concentric end.";
pub const TIP_VEL_LOSS: &str = "The worst (largest) per-rep velocity loss seen this set — the \
     same value the \"Stop set recommended\" banner and the chart's \
     dashed threshold lines are read against. See §9.1/§9.3.";

// -- Velocity-loss interpretation hint (chart + VEL. LOSS tile) ------
/// Worded as guidance, not a verdict: theory.md §9.3 reviews the
/// evidence as directional (low VL better preserves speed/power
/// qualities, high VL favors hypertrophy, strength gains are largely
/// insensitive to threshold) and explicitly calls 20% "a defensible
/// default, not a truth" — so this text offers a coaching cue rather
/// than prescribing a number.
pub const EXPLAIN_VELOCITY_LOSS: &str =
    "The dashed lines mark 10/20/30% velocity loss vs rep 1's mean \
     concentric velocity. Lower loss (≈10-20%) tends to better \
     preserve speed/power qualities; higher loss (>25-30%) is more \
     associated with hypertrophy-style training; strength gains \
     themselves are fairly insensitive to which threshold you pick. \
     20% is a commonly used default, not a rule — see theory.md §9.3 \
     for the evidence and its caveats.";

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_COPY: &[(&str, &str)] = &[
        ("TIP_DEPTH", TIP_DEPTH),
        ("TIP_PEAK_V", TIP_PEAK_V),
        ("TIP_MEAN_V", TIP_MEAN_V),
        ("TIP_LOSS", TIP_LOSS),
        ("TIP_TIME", TIP_TIME),
        ("TIP_REPS", TIP_REPS),
        ("TIP_SET_TIME", TIP_SET_TIME),
        ("TIP_VEL_LOSS", TIP_VEL_LOSS),
        ("EXPLAIN_VELOCITY_LOSS", EXPLAIN_VELOCITY_LOSS),
    ];

    #[test]
    fn every_education_copy_const_is_substantial_prose() {
        for (name, text) in ALL_COPY {
            assert!(
                text.trim().len() >= 40,
                "{name} should be a real explanation, got: {text:?}"
            );
            assert!(
                !text.contains("  "),
                "{name} has a doubled space (string-continuation slip): {text:?}"
            );
        }
    }

    /// The velocity-loss interpretation hint must cite theory.md §9.3 (the
    /// evidence review its coaching-cue framing is grounded in) and mention
    /// the load-bearing terms so a reader can find the depth and isn't left
    /// guessing what the dashed lines mean.
    #[test]
    fn velocity_loss_hint_cites_theory_and_the_chart_lines() {
        assert!(EXPLAIN_VELOCITY_LOSS.contains("§9.3"));
        assert!(EXPLAIN_VELOCITY_LOSS.contains("10"));
        assert!(EXPLAIN_VELOCITY_LOSS.contains("20"));
        assert!(EXPLAIN_VELOCITY_LOSS.contains("30"));
        assert!(EXPLAIN_VELOCITY_LOSS.to_lowercase().contains("default"));
    }

    /// It must read as guidance, not a verdict — theory.md §9.3's own
    /// caveat is that 20% is a default, not a truth; the copy must not
    /// assert loss "is bad" or otherwise prescribe.
    #[test]
    fn velocity_loss_hint_is_a_coaching_cue_not_a_prescription() {
        let lower = EXPLAIN_VELOCITY_LOSS.to_lowercase();
        assert!(!lower.contains("is bad"));
        assert!(!lower.contains("must stop"));
        assert!(!lower.contains("you should"));
    }

    #[test]
    fn tip_mean_v_names_it_as_the_recommended_default() {
        assert!(TIP_MEAN_V.contains("Mean V") || TIP_MEAN_V.contains("MV"));
        assert!(TIP_MEAN_V.contains("§9.2"));
    }

    #[test]
    fn tip_vel_loss_cites_theory() {
        assert!(TIP_VEL_LOSS.contains("§9.1") || TIP_VEL_LOSS.contains("§9.3"));
    }
}
