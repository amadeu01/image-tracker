# Tracking audit — 2026-07-21

Trigger: user review of the 4×6 strategy smoke matrix. Observed failure,
in the user's words: *"it sometimes starts with the center of the bar, but
if the bar disappears it just goes to a random position, or it gets away
from the seed point as the video progresses."*

Reproduced and measured on v3 (`22.55.51`, seed frame 300, on-axis seed
`284,94`, strategy `gaussian:1.5/template`):

| Time | Reported position | Ground truth |
|------|-------------------|--------------|
| 12 s | (272, 207) | correct — on the plate hub, path is a clean vertical column |
| 22 s | (257, 116) | wrong — on the lifter's neck; plate is at ~(295, 80) |
| 30 s | (221, 112) | wrong — drifted onto the rack |
| 38 s | (208, 122) | wrong — parked on rack hardware |

The run reports **100% tracked, 0 gaps, 0 reseeds, mean ZNCC 0.996**.

That combination — total failure at maximum reported confidence — is the
finding. Everything below is an attempt to explain how the code makes it
not merely possible but likely, and what would have to change for it to
become impossible.

This audit deliberately does not propose threshold tuning. Six of the
existing knobs (`min_score`, `update_threshold`, `reacquire_min_score`,
`max_reacquire_distance`, `coast_limit`, `search_radius`) were introduced
by earlier fixes to earlier instances of this same class of bug, and the
failure survived all of them.

## F1 — The model tracks position; the domain is motion

```rust
pub trait Tracker {
    fn step(&mut self, frame: &Frame, last_pos: Point) -> StepOutcome;
}
```
`crates/tracker-core/src/tracker.rs:143`

A tracker is handed one point and one frame. It is not told how much time
has passed, how fast the object was moving, which direction it was going,
or how certain the last estimate was. It therefore cannot know that a
40 px displacement in 16 ms is physically impossible for a loaded barbell,
because it does not know 16 ms have passed, and it has no concept of
physical possibility to appeal to.

The information needed to make that judgement *is* computed by this
codebase — `velocity.rs` derives exactly the velocity state a motion model
would need — but strictly **downstream** of tracking, and it is never fed
back. The pipeline is one-directional by construction:

```
tracking → smoothing → velocity → reps
```

So the system knows, after the fact, that the bar was moving at 0.4 m/s
downward, and separately allows the tracker to teleport it sideways onto a
rack upright. These two facts never meet.

This is the root finding. F2, F3 and F5 are consequences of it.

## F2 — Every guard is conditional on the failure already being visible

`session.rs:242–266` implements two guards, and both are gated on
`self.open_gap_start.is_some()` — i.e. they apply **only when a miss streak
is already open**:

- `max_reacquire_distance` — rejects a `Found` far from the last position
- `reacquire_min_score` — rejects a `Found` scoring below a stricter bar

Both were added in 10.2/10.2b, and both are correct as far as they go.
But they defend a single scenario: *the tracker visibly lost the object,
then tried to re-acquire something implausible.*

The failure we actually have never opens a gap. The tracker never misses.
It slides one or two pixels per frame onto an adjacent high-contrast
structure, scoring above every threshold at every step. In the normal
(non-gap) path there is **no distance guard, no velocity guard, and no
score gate beyond the tracker's own permissive `min_score` of 0.4**.

The guards protect against loud failure. Silent failure is unguarded, and
silent failure is what a correlation tracker on low-texture footage
actually does.

## F3 — `max(anchor, adaptive)` makes the anchor advisory

```rust
let score = match (anchor_score, adaptive_score) {
    (Some(a), Some(b)) => a.max(b),
```
`crates/tracker-core/src/tracker.rs:227`

The anchor template exists, per its own doc comment at `tracker.rs:157`,
to "prevent total drift away from the originally-marked object." The
combinator prevents it from doing that. Under `max`, a candidate the
anchor *rejects* still wins if the adaptive template accepts it. The
anchor can never veto; it can only offer an alternative opinion that is
discarded whenever the adaptive's is higher.

Worse, the refresh is unconditional on anchor agreement (`tracker.rs:244`):
any candidate clearing `update_threshold` — including one that cleared it
purely on the adaptive's score — *becomes* the new adaptive template. Each
small error is written back into the reference and becomes the baseline for
the next frame. This is a ratchet with no restoring force, and it is a
sufficient mechanism on its own to walk the tracker off the object over
~10 seconds, which is what the measurements above show.

The 3.6 design was sound. The combinator inverts it.

## F4 — "Score" is one name for incomparable quantities

`TemplateTracker` returns a ZNCC correlation coefficient. `ColorTracker`
returns a fill-fraction. `compare.rs:8` already documents that these are
different units and refuses to average them:

> the color tracker's "score" is a fill-fraction, a different unit, so it
> is reported separately and never averaged together with template scores

Yet `StepOutcome::Found { score }` erases that distinction at the port
boundary, and `session.rs` then applies `reacquire_min_score` — a single
scalar threshold — to whichever tracker is installed. A value tuned
against correlation is being applied to a fill-fraction. The v3 numbers
show the consequence plainly: template strategies score 0.99 and color
strategies score 0.27 on the *same footage*, and one threshold governs both.

The `Tracker` port leaks its implementation's confidence semantics into
cross-cutting session policy.

## F5 — There is no way to express "tracked, but wrong"

The full vocabulary of tracking outcomes:

- `StepOutcome`: `Found` | `Miss`
- `SessionState`: `Tracking` | `NeedsReseed`
- `Source` (exported): `Tracked` | `Interpolated`

Nothing in that vocabulary can represent low confidence, a stale lock, or
an object that has left the frame. `Found` asserts success. A false lock
is exported as `Tracked`, indistinguishable from a correct sample, and
flows into `velocity_series` → `segment_reps` → `rep_metrics` as fact.

CONTEXT.md's "Gap" term promises metrics "exclude or flag interpolated
samples" — honest handling of data we know is uncertain. There is no
corresponding concept for data we ought to suspect. The honesty guarantee
covers the case the system detects and not the case it doesn't.

The headless CLI compounds this: on `NeedsReseed` it auto-resumes from the
last known position (documented in `docs/e2e-results.md`), which is
precisely the position that is wrong when the cause was drift. The
recovery mechanism re-seeds the failure.

## F6 — A patch of pixels is not the object

`TemplateTracker` identifies the bar by pixel similarity to a remembered
patch. That forces a tradeoff we have now hit from both sides, in one day:

- **Seed off-axis** (plate face, high texture) → good discriminability,
  but the point orbits as the plate spins in the sleeve. Measured on v3:
  r ≈ 35 px ≈ 45 mm of pure rotation artifact, x stdev 69.1 px.
- **Seed on-axis** (chrome sleeve hub, rotation-invariant) → x stdev drops
  to 30.1 px, but the hub is smooth, specular and low-texture, so rack
  uprights and pin holes out-score it. This is the drift documented above.

There is no seed position that is both rotation-invariant and
discriminative, because the tracker's notion of identity is appearance,
and this object's appearance is either rotating or featureless.

The object itself, however, has a property the model entirely ignores: it
is **a circle of known physical diameter**. That is a far stronger and more
stable identity than any patch of pixels, it is rotation-invariant by
definition, and it yields two things we currently ask the user for or do
without — a per-frame pixel→metre scale (competition plate = 0.450 m), and
a genuine lost-detector (fitted radius should be near-constant; a sudden
change means the fit is no longer on a plate).

## F7 — The verification method has failed three times, identically

From PLAN.md's review log and this audit:

| Date | Bug | What the numbers said |
|------|-----|----------------------|
| 2026-07-15 | 3.4/3.5 rotation metadata — tracking scrambled pixel rows | No crash, no NaN, plausible y-ranges |
| 2026-07-16 | 10.9 duplicate frame indices → zero reps exported | Export "succeeded" |
| 2026-07-21 | This — drift onto rack | 100% tracked, 0 gaps, ZNCC 0.996 |

Three times, every available metric reported health while the output was
wrong, and all three were caught only by a human looking at pixels.

This is not bad luck. Every metric the project collects — `tracked_pct`,
`gaps`, `reseeds`, `mean_score`, `mean_jitter_px` — measures the tracker's
**self-assessment**. A confident tracker locked on the wrong object
maximises all of them simultaneously: it never misses, never reseeds, and a
stationary false lock on rigid rack steel has *lower* jitter and *higher*
correlation than correct tracking of a moving barbell.

`compare.rs`'s benchmark, the smoke kit, and CI's e2e assertions all rest
on this family of metrics. **The project currently has no measurement of
correctness at all** — only of confidence. Until it does, every fix
proposed below is unfalsifiable, including the ones in this document.

## Proposed direction

Ordered by depth, not by ease. The ordering is deliberate: measurement
first, because F7 means nothing else can be evaluated.

### 17.1 — Ground-truth fixtures and an accuracy metric (do this first)

Hand-label the true bar-centre position on ~20 sampled frames per test
video (80 labels total), stored as CSV next to the videos. Add a test that
runs a strategy end to end and reports **mean/max Euclidean error against
ground truth, in pixels and in plate-diameters**. Wire it into `compare`
as a column and into CI as a threshold.

This converts every claim in this document, and every fix below, from an
opinion into a number. It also retroactively grades the 24 overlays we
just produced. Without it we are tuning blind, which is how six existing
thresholds were arrived at.

Cost is real (manual labelling) and unavoidable. It is the single highest
-value item here.

### 17.2 — `Track` state with a motion model (fixes F1, F2)

Replace the `last_pos: Point` parameter with a state object carrying
position, velocity and uncertainty, plus `dt`. Predict the next position
from constant velocity; centre the search window on the *prediction*
rather than the last observation; reject observations whose implied
acceleration is physically impossible for a loaded barbell.

This makes drift self-correcting rather than self-reinforcing: a candidate
1 px off-axis per frame is consistent with the motion model, but a
sustained sideways walk while the model expects vertical motion is not.
It also replaces the mid-gap-only distance guard (F2) with one that is
always active, and gives coasting a principled basis — predict through the
occlusion instead of freezing.

A full Kalman filter is not required and probably not warranted; a
constant-velocity predictor with a gating radius derived from measured
barbell dynamics covers this footage. Keep it in `tracker-core`, pure.

### 17.3 — Anchor as veto (fixes F3)

`max(anchor, adaptive)` → accept only if the anchor also clears a floor;
refresh the adaptive only when the anchor agrees. Small change, restores
the 3.6 design's actual intent, and is testable directly with the existing
synthetic-blend tests.

### 17.4 — Confidence as a first-class, tracker-independent concept (fixes F4, F5)

Widen `StepOutcome` to carry a normalised confidence plus a
tracker-specific raw score, and add an explicit `Lost` outcome distinct
from `Miss` (transient) — with a matching `Source::Suspect` (or a
confidence column) in the export, so downstream metrics can exclude or
flag samples the way CONTEXT.md's Gap term already promises for
interpolated ones. Stop auto-resuming from the last position in headless
mode when the cause was drift.

### 17.5 — Plate-circle tracker (fixes F6)

Fit the plate rim per frame (circle fit seeded by the user's click) and
report the fitted centre as the Bar Path position. Rotation-invariant by
construction, removing the seed-placement tradeoff entirely; radius gives
a per-frame scale and a real lost signal. Add as a strategy in the
`compare` matrix so it is measured against Template/Color under 17.1's
accuracy metric rather than assumed better.

### 17.6 — Retire or gate the Color tracker

On all four test videos every color strategy carries the
`suggest_tracker`-indistinct note, and on v3 it posts 0.27 mean
fill-fraction with up to 39 reseeds. It is not a viable strategy for
un-marked footage, and its low jitter actively misleads the benchmark
table. Either gate it behind a positive `suggest_tracker` result or
restrict it to footage with a real Marker, per CONTEXT.md's term.

## Note on sequencing

17.1 before everything. 17.3 is a small correctness fix that can land
alongside it. 17.2 and 17.5 are the substantive changes and should be
evaluated against 17.1's accuracy metric, on the same four videos, before
either is called done — with a human viewing the overlays as well, because
F7's lesson is that the numbers alone have never been sufficient.
