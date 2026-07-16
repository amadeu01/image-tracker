# Theory

The engineering and image-processing reasoning behind image-tracker's
pipeline: why each stage exists, why ZNCC is the matching metric, what noise
we're actually fighting in phone footage, and why some "obvious" fixes
(histogram equalization, sharpening) are wrong for this pipeline. Written for
contributors; assumes familiarity with the vocabulary in
[CONTEXT.md](../CONTEXT.md).

This is a living document. Section 7 (Experiment log) is where empirical
results from `docs/e2e-results.md` and future strategy-benchmark runs (task
11.4) get distilled into durable, dated evidence.

## 1. Pipeline overview

```
 MP4 file
    │  ffmpeg subprocess: decode + rotation (ADR 0001)
    ▼
 Frame (owned RGB buffer, display-space dimensions)
    │  luma = 0.299R + 0.587G + 0.114B  (ITU-R BT.601)
    ▼
 Patch extraction (square region around a point)
    │
    ├── Template Tracker ──▶ ZNCC(anchor|adaptive, candidate) over a
    │                        search window around last known position
    │
    └── Color Tracker ─────▶ HSV match + centroid over a search window
    │
    ▼
 StepOutcome::{Found, Miss}  per frame
    │  Gap logic: coast over short miss runs, interpolate, pause+reseed on long ones
    ▼
 Bar Path (raw positions, Tracked | Interpolated)
    │  centered moving-average smoothing (edge-shrinking window)
    ▼
 Smoothed positions
    │  central finite differences (dt from frame timestamps)
    ▼
 Velocity series (vx, vy, speed; px/s or m/s via Calibration)
    │  sign(vy) segmentation with hysteresis + min phase duration
    ▼
 Reps (eccentric → bottom → concentric)
```

**Decode (ffmpeg, rotation-aware).** Frames come from an `ffmpeg` subprocess
piping rawvideo (see [ADR 0001](adr/0001-shell-out-to-ffmpeg.md)), not a
Rust decoding crate. Phone footage frequently carries a Display Matrix
rotation in `stream_side_data` (e.g. `rotation=-90` for portrait video
stored in a landscape-coded stream); `ffmpeg`'s decoder applies that
rotation automatically to its output, but the *coded* width/height reported
by `ffprobe` is pre-rotation. Every consumer that sizes a `Frame` buffer
must use the *display* dimensions (`display_width()`/`display_height()`,
swapped from coded when rotation is an odd multiple of 90°), or every row is
silently reinterpreted at the wrong stride — the buffer parses without
error, produces plausible-looking numbers, and is completely wrong pixel
data (see §7, 2026-07-15 rotation bug).

**Grayscale conversion.** `patch.rs`'s `luma()` uses
`0.299·R + 0.587·G + 0.114·B` — the ITU-R BT.601 luma weights, matching the
human eye's greater sensitivity to green than red or blue (Recommendation
ITU-R BT.601-7, §2.5.1; the coefficients also appear as "NTSC luma" in
older literature). Everything downstream — `Patch`, `Zncc`, template
matching — operates on this single-channel luma space, never raw RGB: this
keeps template matching correlation cheap (one scalar per pixel) and is
what makes the ZNCC math in §2 exact.

**Region extraction.** `extract_patch` (patch.rs) pulls a square
`(2·radius+1)²` window of luma values around an integer pixel center,
bounds-checked (`None` if any part would fall outside the frame — never a
partial/clamped patch, since a partial patch would silently compare against
a differently-shaped reference).

**Matching.** Two interchangeable `Tracker` implementations (tracker.rs,
color.rs), selected per `TrackingSession` (§2, §4).

**Gap logic** (session.rs). See CONTEXT.md's "Gap": short miss runs coast
(the tracker keeps searching around the last known position) and are
retroactively linearly interpolated once reacquired; a run longer than
`coast_limit` pauses the session (`SessionState::NeedsReseed`) until the
caller re-places the seed.

**Path smoothing → velocity → reps**: §6 below.

## 2. Template matching theory

### SSD vs NCC vs ZNCC

Given a template `T` and a candidate window `I` of the same size (`n`
pixels), three classic similarity/distance measures:

**Sum of Squared Differences (SSD)**

```
SSD(T, I) = Σᵢ (T(i) − I(i))²
```

Cheapest to compute, but not invariant to anything: a uniform brightness
offset (`I = T + c`) or gain change (`I = k·T`) inflates the score even
though the *pattern* hasn't changed at all. Unusable across a rep where
lighting on the plate shifts as it rotates.

**Normalized Cross-Correlation (NCC)**

```
NCC(T, I) = Σᵢ T(i)·I(i)  /  sqrt(Σᵢ T(i)² · Σᵢ I(i)²)
```

Invariant to uniform *gain* (`I = k·T`, `k > 0`) but not to an additive
brightness *offset* — a constant added to every pixel changes the score.

**Zero-mean Normalized Cross-Correlation (ZNCC)** — what this repo uses
(`metric.rs`'s `Zncc`):

```
ZNCC(T, I) = Σᵢ (T(i) − T̄)(I(i) − Ī)  /  sqrt( Σᵢ(T(i) − T̄)² · Σᵢ(I(i) − Ī)² )
```

where `T̄`, `Ī` are the mean luma of the template and candidate patch. This
is exactly the Pearson correlation coefficient between the two patches'
pixel values, so its range is `[-1, 1]`; identical patches score `1.0`, and
a constant (zero-variance) patch is defined as `0.0` here rather than
dividing by zero (`metric.rs` tests: `zncc_of_constant_patch_is_zero_not_nan`).

**The affine-invariance proof — "how do we use contrast".** Let
`I(i) = a·T(i) + b` for any `a > 0`, `b ∈ ℝ` (a positive affine transform:
gain `a` plus offset `b`, i.e. brightness/contrast change). Then:

```
Ī = a·T̄ + b
I(i) − Ī = a·(T(i) − T̄)
```

Substituting into ZNCC's numerator and denominator:

```
numerator:   Σᵢ (T(i) − T̄) · a(T(i) − T̄) = a · Σᵢ (T(i) − T̄)²
denominator: sqrt( Σᵢ(T(i) − T̄)² · Σᵢ a²(T(i) − T̄)² ) = a · Σᵢ (T(i) − T̄)²   (a > 0)

ZNCC(T, I) = [a · Σᵢ(T(i) − T̄)²] / [a · Σᵢ(T(i) − T̄)²] = 1.0
```

The gain `a` cancels top and bottom (subtracting the mean removes the
offset `b` before the ratio is even taken), so ZNCC is *exactly* 1.0 for
any positive affine transform of the template — not approximately, not
"tends to be robust", but algebraically invariant. This is the direct
justification for tracking a plate end-face through a rep even as ambient
lighting brightens/dims and local contrast shifts with rotation: as long as
the appearance change on the patch is well-approximated by an affine
transform of luma, the match score is unaffected. `metric.rs`'s test
`zncc_is_invariant_to_brightness_and_contrast_change` exercises this
directly (`b = 0.5·a + 10`, i.e. `a=0.5, b=10` in the notation above) and
asserts the resulting score is `1.0` to within `1e-4` (numerical, from the
byte round-trip through `Frame`, not floating-point-exact).

This does *not* cover non-affine appearance change (genuine rotation of a
3D object revealing a different texture, specular highlights, occlusion) —
that's what the dual-template design in §3 is for.

### References

- J. P. Lewis, ["Fast Normalized Cross-Correlation"](https://scribblethink.org/Work/nvisionInterface/nip.pdf),
  Vision Interface, 1995, pp. 120–123. The standard reference for computing
  NCC efficiently via running sums/integral images; also lays out the
  SSD/NCC/ZNCC distinctions used above. (Verified: PDF hosted at the
  author's site, scribblethink.org; also indexed on
  [Semantic Scholar](https://www.semanticscholar.org/paper/Fast-Normalized-Cross-Correlation-Lewis/b482ddd4ddfdd33a709f7f0663d3e5c116ff4d52).)
- B. Pan, K. Qian, H. Xie, A. Asundi,
  ["Two-dimensional digital image correlation for in-plane displacement and strain measurement: a review"](https://iopscience.iop.org/article/10.1088/0957-0233/20/6/062001),
  *Measurement Science and Technology*, 20(6), 062001, 2009,
  doi:[10.1088/0957-0233/20/6/062001](https://doi.org/10.1088/0957-0233/20/6/062001).
  Surveys correlation criteria (including ZNCC) and sub-pixel refinement for
  digital image correlation — the same underlying math as visual template
  tracking, from the experimental-mechanics literature. (Verified via
  IOPscience.)
- L. G. Brown, ["A survey of image registration techniques"](https://dl.acm.org/doi/10.1145/146370.146374),
  *ACM Computing Surveys*, 24(4), 325–376, 1992,
  doi:[10.1145/146370.146374](https://doi.org/10.1145/146370.146374).
  Broader context: where correlation-based matching (this repo's approach)
  sits among feature-based and transform-domain registration methods.
  (Verified via ACM Digital Library and a hosted PDF at
  [sci.utah.edu](https://www.sci.utah.edu/~gerig/CS6640-F2010/p325-brown.pdf).)

## 3. Seed → template ("pinpointing")

The user's seed click (CONTEXT.md's "Seed") becomes the anchor patch:
`TemplateTracker::new` extracts a `(2·patch_radius+1)²` luma patch centered
on the rounded seed pixel from the seed frame (`tracker.rs`). This single
reference patch is stored twice — as `anchor` and as the initial `adaptive`
— and both are used every subsequent step.

**Why dual templates.** A single fixed template (anchor-only) is exactly
the classical Lucas-Kanade tracking failure mode: as the tracked surface
rotates or lighting shifts, the live appearance diverges from the seed-time
appearance until the match score falls below `min_score`, even though
nothing is actually lost — it's the *reference* that's stale. Naively
replacing the template with the latest match every frame is the opposite
failure: template drift, where each update introduces a small alignment
error that compounds frame over frame until the tracked point has crept
entirely off the object (Matthews, Ishikawa & Baker 2004, below — this is
literally titled "the template update problem").

`tracker.rs`'s `TemplateTracker` resolves this with three pieces working
together:

1. **Anchor** (never updated) — the ground truth of what the user actually
   marked. No matter how long the clip runs, a candidate must still score
   reasonably against the *original* appearance to be findable at all when
   the adaptive template has drifted or been lost. Prevents unbounded
   drift.
2. **Adaptive** (updated from the winning match) — absorbs gradual, real
   appearance change (rotation, lighting) that would otherwise erode the
   anchor-only score below `min_score` over a long clip. Per step, each
   candidate's effective score is `max(anchor_score, adaptive_score)`.
3. **`update_threshold`** — the adaptive template is only replaced when the
   *winning* effective score clears this threshold (default `0.7`,
   comfortably above `min_score`'s default `0.4`). A marginal match
   (occlusion edge, near-miss, background clutter that just clears
   `min_score`) is still accepted as `Found` — the tracker doesn't lose the
   object over a single weak frame — but the adaptive template is not
   allowed to creep toward that marginal evidence. This is the direct
   mechanism preventing "drift by creep": every adaptive-template update is
   gated on genuinely confident evidence, not just "good enough to count as
   found."

This is TDD'd directly: `tracker.rs`'s
`dual_template_stays_found_through_gradual_appearance_change_that_would_lose_anchor_alone`
walks a synthetic non-affine appearance blend from t=0 to t=1 in small
per-frame steps and confirms (a) the dual-template tracker stays `Found`
throughout and (b) the anchor patch scored directly against the final
appearance really has dropped below `min_score` — proving the adaptive
template is doing real work, not just padding coverage.
`marginal_match_below_update_threshold_does_not_refresh_adaptive_template`
proves the threshold gate holds: a repeated marginal-score frame reproduces
the identical score, meaning the adaptive template was never touched by it.

### Reference

- I. Matthews, T. Ishikawa, S. Baker,
  ["The Template Update Problem"](https://www.ri.cmu.edu/pub_files/pub4/matthews_iain_2004_1/matthews_iain_2004_1.pdf),
  *IEEE Transactions on Pattern Analysis and Machine Intelligence*, 26(6),
  810–815, 2004, doi:[10.1109/TPAMI.2004.16](https://doi.org/10.1109/TPAMI.2004.16).
  Formalizes exactly the drift failure mode the anchor+adaptive design
  above is built to avoid. (Verified: PDF hosted at CMU Robotics
  Institute; also on [IEEE Xplore](https://ieeexplore.ieee.org/document/1288530/).)

## 4. Color tracking

`color.rs`'s `ColorModel` is the alternative to template matching, used
when a physical `Marker` (CONTEXT.md) is present on the bar. It samples the
patch around the seed, converts every pixel to HSV
(`rgb_to_hsv`, standard formula: hue via the max-channel case split,
saturation as `(max−min)/max`, value as `max`), and learns:

- **hue** — the circular mean of sampled hues (`median_angle_deg`: sum unit
  vectors, take the angle of the resultant). A plain numeric median would
  be pulled toward 180° for a hue cluster straddling the 0°/360° wrap
  (reds); the circular mean handles this correctly (tested:
  `learn_handles_hue_wraparound_near_red`).
- **saturation** and **value** — plain numeric medians (no wraparound
  issue: both are linear `[0, 1]` ranges).

`ColorModel::matches(rgb)` then checks a pixel against tolerance bands
around each of the three learned values (`hue_tolerance` uses the
wraparound-safe `hue_distance_deg`; saturation/value are plain absolute
difference). The `ColorTracker` (built on this) scans the search window for
matching pixels and reports their centroid as the tracked position.

**Saturation/value floors matter more than they look.** A model learned
from a low-saturation (gray/washed-out) patch is nearly useless as a
selectivity filter — hue is mathematically undefined at zero saturation
(a hue rotation of pure gray doesn't change its RGB at all), so the
saturation and value tolerance bands are what actually keeps a color model
from matching half the frame. `matches_rejects_low_saturation_gray_against_saturated_model`
and its converse (`matches_a_gray_model_rejects_saturated_pixel`) both
exist because either direction (a vivid marker vs. a gray background, or a
gray-tape marker vs. a saturated background) needs the saturation/value
bands, not hue alone, to discriminate.

**When color beats template — and why it doesn't, here.** Color tracking
is the right tool when the tracked object has a color that's genuinely
distinct from its surroundings: a bright marker dot against skin, clothing,
or gym equipment. It is *cheaper* per pixel (HSV conversion + three range
checks vs. a correlation over a whole patch) and immune to the object's own
rotation/appearance change entirely, since it only cares about color, not
shape. But this repo's advisor findings (Marker Color Advisor,
CONTEXT.md) and the actual test footage in `test_videos/` are, in practice,
gyms — chrome plates, black rubber, gray flooring, muted rack paint. There
is no reliably distinct color signature to lock onto without a marker the
lifter physically applies, which most users filming a casual set won't
have done. That's why the Template Tracker is the primary path and the
Color Tracker is the marker-present alternative, not the default (see
CONTEXT.md's "Marker": "Optional: when present, the Color Tracker follows
it; when absent, the Template Tracker follows the bar's own appearance").

## 5. Noise

### Sources in our footage

- **Sensor grain** — ordinary phone-camera photon/read noise, worse in dim
  indoor gym lighting (long or gain-boosted exposure). Manifests as
  independent per-pixel luma jitter frame to frame.
- **WhatsApp H.264 compression blocking** — every video in `test_videos/`
  arrived as a WhatsApp export, which re-encodes at an aggressive bitrate.
  Block-DCT compression introduces visible 8×8/16×16 blocking artifacts and
  ringing near edges, especially on the plate's high-contrast edge against
  the background — exactly where a template patch's boundary sits.
- **Motion blur at rep turnaround** — cheapest here: shutter speed on a
  phone doesn't freeze fast bar motion, so frames near peak concentric/
  eccentric velocity smear the tracked edge across several pixels, while
  frames at the top/bottom turnaround (near-zero velocity) are comparatively
  sharp. This is asymmetric noise: it's worst exactly where the object is
  moving fastest, not uniform across the clip.

### Why noise hurts correlation, and how much ZNCC tolerates

A correlation-based matcher's failure mode from noise is a **false peak**:
independent per-pixel noise on both the reference and candidate patches
adds an uncorrelated component to the numerator's cross term while also
inflating both variance terms in the denominator, pulling every candidate's
score down and — critically — narrowing the score gap between the true
match and a nearby, structurally similar wrong match (e.g. a rack upright
that shares the plate's grayscale range). Below some noise level the wrong
candidate can outscore the true one within the search window.

ZNCC tolerates a meaningful amount of this because it's a *normalized*
statistic — its score depends on the correlation *coefficient* between
patches, not on absolute pixel differences, so it degrades gracefully
(monotonically lower peak score) rather than catastrophically (SSD, by
contrast, has no built-in resistance to a global brightness/contrast shift
riding on top of noise, compounding both failure modes at once). But ZNCC
is not immune: it is a *linear* correlation measure, so it has no special
robustness to blocking artifacts (which are structured, not i.i.d., noise)
or to motion blur (which changes the underlying signal, not just adds noise
to it) — both are why the anchor+adaptive design in §3 exists, rather than
leaning on ZNCC's noise tolerance alone to survive a whole rep.

### Filter theory

- **Gaussian blur** — a linear, isotropic low-pass filter: convolution with
  a Gaussian kernel attenuates high-spatial-frequency content
  (frequency-domain argument: the Fourier transform of a Gaussian is itself
  a Gaussian, so it's a smooth low-pass with no ringing). Effective against
  independent per-pixel sensor grain (which is broadband/high-frequency)
  but blurs real edges too — including the plate boundary the tracker
  actually wants sharp, and it has no special handling for blocking
  artifacts, which are itself already a form of low-frequency-per-block
  distortion that a smooth Gaussian doesn't specifically target.
- **Median filter** — a nonlinear, order-statistic filter: replaces each
  pixel with the median of its neighborhood. Nonlinear filters can remove
  outlier noise (salt-and-pepper, and — usefully here — some blocking
  artifact edges) while preserving genuine step edges much better than a
  linear blur, since the median of a neighborhood straddling a real edge is
  still one of the two "sides'" values, not an interpolated blend.
  Comparatively worse against pure Gaussian sensor grain (no principled
  frequency-domain justification, and it can introduce its own
  "stair-stepping" artifacts on smooth gradients).

Both are candidates for the `Preprocessor` port (CONTEXT.md; task 11.2):
which one (or chain) helps depends on which noise source dominates a given
clip, which is exactly why Tracking Strategy (CONTEXT.md) is made
per-video-swappable rather than hardcoded — task 11.4's strategy benchmark
is what will make this an empirical choice rather than a guess.

### Why not CLAHE/histogram equalization, and why not sharpening

**CLAHE/equalization is redundant under ZNCC's invariance.** Histogram
equalization (and its adaptive/local variant, CLAHE) exists to correct for
uneven brightness/contrast across an image or between two images being
compared. But §2 proved ZNCC is *already* exactly invariant to any global
affine transform of a patch's luma values — precisely the class of
brightness/contrast difference equalization is designed to normalize away.
Running CLAHE before ZNCC spends compute correcting for something the
matching metric already ignores, and — worse — CLAHE is itself a
*nonlinear, spatially-local* transform (it operates per-tile, with
contrast limiting), which means it does not commute with taking the ZNCC
score analytically the way a global affine map does: it can introduce
spurious local contrast structure (halos at tile boundaries, amplified
noise in low-contrast regions like flat rubber flooring) that isn't
present in the source and that isn't cancelled by ZNCC's invariance the
way real affine brightness change is. It's not neutral — it's actively
liable to hurt by manufacturing texture noise ZNCC then has to score
against.

**Sharpening is counterproductive** because unsharp-masking amplifies high
spatial frequencies — exactly the frequency band sensor grain and
compression-block edges occupy. It increases local contrast at genuine
edges, but noise doesn't distinguish itself from a genuine edge to a
sharpening kernel; the net effect on a phone-video patch is amplified
grain and exaggerated blocking-artifact edges, which is the opposite of
what the matcher needs (a stable numerator, tight variance in the
denominator).

### Region-level filtering + the same-space invariant

CONTEXT.md's Preprocessor term is explicit about the constraint that
actually matters here: whatever filter chain runs, it must be applied
identically to the seed's reference patch and to every candidate region —
"reference and candidates must live in the same filtered space for scores
to be comparable." This is not a nicety; it's required for ZNCC's
invariance proof in §2 to hold at all. That proof assumes `T` and `I` are
compared directly; if `T` were filtered with a different kernel (or not
filtered at all) than `I`, the two patches are no longer related by even an
approximate affine transform of the same underlying signal, and ZNCC's
score becomes an artifact of the filter mismatch rather than a measure of
how well the candidate matches the tracked object. Filtering at the
*region* level (after `extract_patch`, before `Zncc::score`) rather than
once on the whole decoded frame is also what keeps this invariant cheap to
guarantee: every comparison point in the pipeline has exactly one
well-defined filtered representation of its patch, with no whole-frame
cache to keep in sync with per-call filter-chain choices (task 11.2's
`Preprocessor` port is designed around this).

## 6. Positional smoothing + kinematics

`smoothing.rs`'s `smooth_positions` runs a centered moving average over the
raw Bar Path *before* `velocity.rs` differentiates it — never the reverse.
This ordering matters because finite differentiation is itself a
high-pass, noise-amplifying operation: for a fixed per-frame position noise
of standard deviation `σ`, a first difference `(x[i+1] − x[i])/dt` has
variance `2σ²/dt²` — noise is amplified by `dt⁻¹` (and further whenever
`dt` is small, i.e. at high frame rate), while any real underlying signal
of that frequency is amplified by the same factor along with it, so raw
tracking jitter of a few pixels turns into velocity spikes of tens of
px/s once divided by a 1/30s frame interval. `velocity.rs`'s own test
`smooths_before_differentiating` demonstrates this directly: a zigzag
position noise with variance-reduced-by-smoothing produces
correspondingly reduced velocity variance, without changing the underlying
linear trend's slope (smoothing a linear ramp reproduces it exactly —
`smoothing.rs`'s `linear_ramp_is_preserved_exactly_by_centered_average` —
so the smoothing doesn't bias the true velocity estimate, it only damps the
noise riding on top of it).

Differencing itself uses a **central difference** at interior points,
`(p[i+1] − p[i−1]) / (t[i+1] − t[i−1])`, and a one-sided (forward/backward)
difference at the two series endpoints where no interior neighbor exists.
Central differencing is second-order accurate (error `O(dt²)`) versus
first-order (`O(dt)`) for a one-sided difference, which is why it's
preferred everywhere it's available.

**VBT mean-velocity standard.** Velocity-based training tools (GymAware,
Vitruve, and the like — see the README's "Why") report *mean concentric
velocity* as their primary metric, because peak velocity within a rep is
sensitive to exactly where the instantaneous sample lands and to
measurement noise, while the mean over the whole concentric phase is a
stable, load-comparable number lifters use to autoregulate training
intensity. `rep.rs`'s `segment_reps` produces the `eccentric_start` /
`bottom` / `concentric_end` indices into the velocity series specifically
so that per-rep metrics (mean and peak concentric velocity, depth) can be
computed over the correctly bounded concentric phase — matching the metric
definition this domain's target users (lifters, per the README) already
expect from commercial VBT devices.

## 7. Experiment log

Empirical results and tuning decisions, most recent first. Full detail and
reproduction commands live in [docs/e2e-results.md](e2e-results.md); this
table is the durable, at-a-glance summary. Future strategy-benchmark runs
(task 11.4, comparing filter × tracker-kind combinations) land here too.

| Date | Experiment | Finding | Theory link |
|------|-----------|---------|--------------|
| 2026-07-15 | 10.2b — `reacquire_min_score` decoupled from `update_threshold` and swept 0.4–0.7 on v1 | 0.7 (=`update_threshold`) over-rejected genuine marginal reacquisitions (2/0 → 8/7 gaps/reseeds). 0.5 chosen: best gaps/reseeds (8/6) without reproducing the pre-10.2 false-lock risk that 0.4 (=`min_score`, a no-op gate) would. Tracked-sample max jump was identical (42.4px = `search_radius`×√2) across the whole sweep — no reacquisition in this clip ever locked onto something outside the geometric search window regardless of score threshold. | §3 (adaptive template update gating) is the analogous idea one level up: gate confidence separately from bare "found" acceptance. |
| 2026-07-15 | 3.6 — dual-template (anchor + adaptive) tracking, re-run with visually re-picked seeds on all 4 test videos | v3/v4: 0 gaps, 0 reseeds (unaffected — anchor alone was already sufficient). v1: improved over 3.5 (2 gaps vs 3 reseeds, different seeds so not fully controlled). v2: honest negative result by the numbers (6 reseeds, 26 interpolated) — visual review was needed to judge whether the dual-template change actually helped, since gap/reseed counts alone don't distinguish "same tracker, harder clip" from "regression." Synthetic per-pixel blend tests (non-affine, since a uniform brightness ramp doesn't exercise the adaptive path under ZNCC's own invariance) confirmed the anchor-only tracker really would have lost the object where the dual-template one didn't. | §2 (why a uniform ramp doesn't test anything — ZNCC already handles that); §3 (anchor/adaptive/update_threshold design) |
| 2026-07-15 | 3.4/3.5 — rotation metadata bug found via visual (not CSV) review | Phone-captured v1/v2 carry `rotation=-90` Display Matrix side data; `ffmpeg` auto-rotates its decoded output but the pipeline sized `Frame` buffers from ffprobe's *coded* (pre-rotation) dimensions, silently reinterpreting every row at the wrong stride. Produced plausible-looking CSV output (no crash, no NaN, plausible y-ranges by coincidence) while tracking scrambled pixel data end to end — only caught by extracting and visually inspecting decoded frames. | §1 (decode/rotation); general lesson: CSV/numeric plausibility alone is an insufficient verification method for an image pipeline (see PLAN.md's review log, 2026-07-15 and 2026-07-16 entries, for the same pattern recurring with 10.9's duplicate-frame bug) |
| 2026-07-16 | 11.4 — strategy benchmark (`tracker-app compare`): {none, gaussian:1.5, median:3} × {template, color}, 200-frame segments on v1 (seed frame 789 @ 312,430) and v3 (seed frame 300 @ 260,120) | v1: all 6 strategies track ≥98.5%; winner `gaussian:1.5/color` (100% tracked, 0.81px jitter) by the tie-break rule, but every color strategy carries the `suggest_tracker`-indistinct note (v1's marker isn't color-distinct from its background) — color's near-zero jitter here is an artifact of a coarse blob centroid over a small, mostly-static window on this segment, not evidence the color model is actually locking onto the right pixels; `gaussian:1.5/template` (100% tracked, 6.95px jitter) is template-tracking's own best filter and the safer pick given the indistinct-color caveat. v3: winner `none/template` (100% tracked, 0.23px jitter, mean correlation 0.987) — no filtering needed at all; color strategies again get the indistinct-color note and post visibly worse jitter (0.62–0.65px) and lower mean fill-fraction than every template variant. Across both clips, Gaussian blur nudged template correlation up slightly (0.961→0.973 on v1, 0.987→0.996 on v3) without moving tracked% (already ≥98.5% unfiltered on these two relatively clean clips); median:3 was consistently the weakest template filter of the three (lowest correlation each time), though still never worse on tracked%. Full tables: `out/compare/v1.json`, `out/compare/v3.json`. | §5 (filter theory: why Gaussian/median move score without moving the pipeline's correctness guarantee); the color-strategy caveat is `suggest.rs`'s own heuristic (4.3) surfacing here — a "winning" jitter number from a tracker `suggest_tracker` itself would have steered away from is a reason to read `compare`'s notes column, not just its numbers, echoing 3.4/3.5/10.9's recurring lesson above that a clean-looking metric isn't the same as a correct one. |
