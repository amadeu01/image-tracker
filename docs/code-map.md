# Code Map — how to read this codebase

A guided tour for someone opening this repo for the first time. It answers:
where does a tracking run start, how does a frame become a velocity number,
how does the GUI talk to the domain, and which file do I open to change X.

Companion docs — read in this order if you're new:
1. [CONTEXT.md](../CONTEXT.md) — the vocabulary (Bar Path, Seed, Gap, Lost, Rep…). Every term is a real type name.
2. **This file** — the mental model and the navigation map.
3. [docs/architecture.md](architecture.md) — the layer rules, DDD application, dependency graph.
4. [docs/theory.md](theory.md) — the maths (ZNCC, smoothing, velocity) and the sports science.

---

## 1. The one-paragraph mental model

A **worker thread** decodes the video one frame at a time and, for each frame,
asks a **tracker** "where did the bar go?" — a small template-matching search
scored by **ZNCC**. A **session state machine** decides whether to trust each
answer (accept it, coast over a short gap, or pause for a reseed). Accepted
positions accumulate into the **`BarPath` aggregate** — a time-stamped list of
points. Once the run finishes, `BarPath` is pure input to offline maths:
**velocity** (difference neighbouring points ÷ time), **reps** (segment by
up/down direction), and **exports** (CSV/overlay). The **GUI never touches the
tracker directly** — it spawns the worker and reads progress messages off a
channel. That thread split is deliberate (the render loop must never block).

```mermaid
graph LR
    subgraph GUI["GUI thread (egui update loop)"]
        A["AppState<br/>start_tracking()"]
        P["poll_tracking()<br/>drains messages"]
    end
    subgraph Worker["Worker thread"]
        W["decode → track → emit"]
    end
    A -->|spawn + TrackingJob| W
    W -->|"channel: Progress / Done / Error"| P
    P --> BP["BarPath"]
    BP --> Der["velocity · reps · export"]
```

---

## 2. How the GUI talks to the core (the boundary)

Three layers, dependencies point inward (see architecture.md §3):

```
egui panels  ─read/write─►  AppState  ─spawns─►  worker thread  ─calls─►  tracker-core (domain)
(app/*.rs)                 (app/state/*)         (tracking.rs)            (session.rs, tracker.rs, …)
```

- **The GUI never calls `Tracker::step` or `TrackingSession::step` directly.**
  It calls `AppState::start_tracking()`, which spawns the worker and returns a
  `TrackingHandle` (a channel sender + receiver).
- Communication is **one channel, three message types** (`tracking.rs:254`):

```rust
pub enum TrackingMessage {
    Progress { video_frame_index, position, source, state },  // one per frame
    Done(BarPath),                                            // run finished, here's the result
    Error(String),                                            // ffmpeg/decode failure
}
```

- Every UI frame, `poll_tracking()` (`app/state/jobs.rs:166`) drains the channel
  with `try_recv` (never blocks), advances the displayed frame, and on `Done`
  stores the `BarPath` and kicks off `SessionResults` derivation.

**Why a thread at all:** a squat is thousands of frames; decoding + tracking
takes seconds. Doing it in the egui `update()` loop freezes the window. The
worker owns the ffmpeg subprocess and the decoder; the GUI only ever sees
messages. Full rules: [docs/gui-threading.md](gui-threading.md).

### The mirror image: the display decoder

Separate from tracking, scrubbing the video needs single frames on demand.
That's a *second* worker (`decode_worker.rs`) with its own tiny 16-frame cache.
Don't confuse the two:
- `tracking.rs` worker = sequential decode for a whole run (fast, streaming).
- `decode_worker.rs` = random-access single-frame decode for the scrub bar.

---

## 3. The domains (this app bridges two)

This is a barbell-velocity tracker, so its ubiquitous language spans **computer
vision** and **strength training**. Half the vocabulary comes from each — that's
correct, not confusion (architecture.md §4).

| Domain | Terms | Where |
|--------|-------|-------|
| Computer vision | Seed, Template, ZNCC, Patch, Gap, Occlusion, Drift, Calibration, Marker | `tracker.rs`, `metric.rs`, `patch.rs`, `session.rs` |
| Strength training (VBT) | Rep, eccentric/concentric, depth, mean/peak velocity, velocity loss | `rep.rs`, `rep_metrics.rs`, `velocity.rs` |

**Standard terms** (ZNCC, jitter, drift, ground truth, calibration) any CV
engineer knows — no glossary needed. **Project-local coinages** (anchor veto,
NeedsReseed, Lost, coasting, Marker Color Advisor) are defined in CONTEXT.md —
if you'd have to explain it to a CV hire, it lives there.

---

## 4. The state machine

`TrackingSession` (`session.rs`) is the behavioural core. Three states
(`session.rs:271`):

```rust
pub enum SessionState {
    Tracking,      // normal: following the bar
    NeedsReseed,   // recoverable pause: lost the bar too long, waiting for a human/CLI to re-point it
    Lost,          // terminal: "tracked but wrong" for too long — OPT-IN, default OFF
}
```

```mermaid
stateDiagram-v2
    [*] --> Tracking: seed placed
    Tracking --> Tracking: Found (accepted)
    Tracking --> Tracking: Miss (short gap, coast + interpolate)
    Tracking --> NeedsReseed: gap exceeds coast_limit
    NeedsReseed --> Tracking: reseed (new position supplied)
    Tracking --> Lost: sustained low-confidence Founds<br/>(only if lost_detection ON)
    Tracking --> [*]: video EOF → Done(BarPath)
    NeedsReseed --> [*]: user Stop → Done(partial BarPath)
```

Key design decisions (each has scars behind it):
- **`NeedsReseed` vs `Lost`** — a *recoverable pause* asks for a new seed and
  continues; a *terminal* state throws the rest of the set away. The
  distinction is the whole of PLAN 17.4b.
- **`Lost` is default OFF.** Confidence of a *correct* track on a shiny plate
  (~0.3–0.6) and a *false lock* (~0.4–0.46) overlap — confidence can't reliably
  decide termination, so by default it doesn't. A genuine loss pauses via
  `NeedsReseed` instead of dead-ending.
- **Coasting** — a short `Miss` streak doesn't stop the run; the session
  interpolates across the gap and keeps going, marking those points
  `Source::Interpolated` (so velocity can exclude them honestly).

---

## 5. The main algorithm, box by box

### 5.1 The run loop — `tracking.rs`

`run_tracking_worker` (`tracking.rs:594`) sets up ffmpeg, decodes to the seed
frame, builds the tracker, then `finish_tracking_run` → `run_tracking_loop`
does the cycle:

```rust
loop {
    // 1. honour Pause/Stop before touching the next frame
    match control_rx.try_recv() { Stop => return Stopped, Pause => …, _ => {} }

    // 2. pull ONE frame (streaming — never buffer the whole video)
    match source.next_frame()? {
        Some(frame) => {
            session.step(&frame, dt);   // the decision (5.2 + 5.3)
            tx.send(TrackingMessage::Progress { … });   // tell the GUI
        }
        None => return Ok(LoopOutcome::Completed),   // EOF → build BarPath
    }
}
```

`dt` = seconds per frame = `1/fps`, fed to every `step` so the motion model
knows how much time passed.

### 5.2 The search — `tracker.rs`, `TemplateTracker::step`

"Where did the bar go?" = guess with the motion model, scan a box around the
guess, score each spot with ZNCC, keep the best.

```rust
let predicted = track.predicted(dt);          // motion model's guess
let r = self.config.search_radius;
let mut best = None;
for dy in -r..=r {
    for dx in -r..=r {
        let candidate = /* patch at predicted + (dx,dy) */;
        let anchor_score   = metric.score(&self.anchor,   &candidate); // vs ORIGINAL seed
        let adaptive_score = metric.score(&self.adaptive, &candidate); // vs recent appearance
        let score = anchor_score.max(adaptive_score);
        if score > best_score { best = Some((position, score, …)); }
    }
}
match best {
    Some((position, score, …)) if score >= min_score => Found { position, … },
    _ => Miss,
}
```

- **ZNCC** (`metric.rs`, `Zncc::score`) = zero-mean normalized cross-correlation:
  patch similarity from −1..+1, ignoring brightness/contrast. Textbook CV metric.
- **anchor vs adaptive** = the anchor veto (PLAN 17.3): matching against the
  *original* seed (anchor) as well as the recent look (adaptive) resists slow
  drift onto the rack. The anchor score must clear `anchor_floor` for a
  candidate to be eligible at all.

```mermaid
graph LR
    Pred["predicted spot"] --> Box["scan ±search_radius"]
    Box --> Z["ZNCC each candidate<br/>(metric.rs)"]
    Z --> Best["best score"]
    Best -->|≥ min_score| F["Found(position)"]
    Best -->|< min_score| M["Miss"]
```

### 5.3 The trust layer — `session.rs`, `TrackingSession::step`

A raw `Found` isn't accepted blindly. Mid-gap guards demote a suspicious match
back to `Miss` (catches confident-but-wrong locks):

```rust
let outcome = match outcome {
    Found { position, .. } if in_gap && distance(last_pos, position) > max_reacquire_distance => Miss,
    Found { score, .. }    if in_gap && score < reacquire_min_score                            => Miss,
    other => other,
};
match outcome {
    Found { position, identity_confidence, .. } => {
        self.track = self.track.observed(position, dt);   // feed the motion model
        self.samples.push(Sample { frame_index, position, source: Tracked, confidence });
    }
    Miss => { /* extend gap; if too long → NeedsReseed */ }
}
```

### 5.4 The storage aggregate — `bar_path.rs`

Accepted samples + gaps become the `BarPath`. This is **the seam**: tracking
ends here, all maths begins here.

```rust
pub struct PathPoint {
    pub frame_index: u64,
    pub t_seconds: f64,      // frame_index × fps_den/fps_num  — time lives here
    pub position: Point,     // pixels
    pub source: Source,      // Tracked / Interpolated / Seed
}
pub struct BarPath { points: Vec<PathPoint>, gaps: Vec<Gap>, /* + timebase */ }
```

Note: a `PathPoint` holds **position + time, not velocity**. Velocity is derived
later.

### 5.5 Velocity — `velocity.rs` (NOT a sum of vectors)

Common misconception: there is no accumulation of little direction vectors.
Velocity = **derivative of position** = difference neighbouring points ÷ time
between them (central finite difference).

```rust
let smoothed = smooth_positions(points, window)?;   // 1. de-noise first
for i in 0..n {
    let (lo, hi) = if i == 0 {(0,1)} else if i == n-1 {(n-2,n-1)} else {(i-1,i+1)};
    let dt = smoothed[hi].t_seconds - smoothed[lo].t_seconds;
    let dx = smoothed[hi].position.x - smoothed[lo].position.x;
    let dy = smoothed[hi].position.y - smoothed[lo].position.y;
    let vx = scale(dx) / dt;              // scale() = px→metres IF Calibration present
    let vy = scale(dy) / dt;
    let speed = (vx*vx + vy*vy).sqrt();
}
```

1. **Smooth first** — differencing raw jitter amplifies noise into fake spikes.
2. **Central difference** — `(next − prev) / Δt`.
3. **`scale()`** — with a `Calibration` (you clicked both plate edges),
   `px_to_meters` converts to **m/s**; without, stays **px/s**. Same code, unit
   decided by presence of calibration.

### 5.6 Reps and averages — `rep.rs`, `rep_metrics.rs`

"Average bar velocity" is **per rep**, not per video. `segment_reps` splits the
velocity series into eccentric/concentric phases by the sign of `vy` (down vs
up); `rep_metrics` aggregates each concentric phase:

- **Mean velocity** = concentric displacement ÷ concentric duration (the VBT default)
- **Peak velocity** = max `speed` in the phase
- **Velocity loss** = drop from rep 1's mean to the worst rep (fatigue proxy)

```mermaid
graph TD
    BP["BarPath.points"] --> SM["smooth (velocity.rs)"]
    SM --> FD["central difference → VelocitySample"]
    FD --> Seg["segment_reps by sign(vy)<br/>(rep.rs)"]
    Seg --> RM["per-rep mean/peak/loss<br/>(rep_metrics.rs)"]
    RM --> UI["Results panel table + chart"]
```

---

## 6. Confidence vs correctness — the load-bearing lesson

The single most important thing to internalise (milestone 17):

> **The tracker's self-metrics measure confidence, not correctness. A confident
> false lock maxes them all.**

- **Self-metrics** (ZNCC score, tracked %, jitter, gaps/reseeds) are computed
  from the tracker's *own output*. They are legitimate *runtime* signals — the
  tracker needs them live to decide "coast this gap? pause? trust this match?"
- **They cannot certify correctness.** A blob gliding across a wall has
  *beautiful* low jitter while tracking nothing. That's why the `compare`
  benchmark once recommended a failing tracker (PLAN 20.3).
- **The only referee is ground truth.** `groundtruth/*.csv` = human-labelled
  real positions; the `grade` subcommand scores a run against them. Any tracking
  change is validated by re-grading (`scripts/smoke-report.sh` in CI), never by
  watching a self-metric improve.

Two jobs, both kept: self-metrics *steer* the tracker at runtime; ground truth
*judges* it offline.

---

## 7. Navigation map — "I want to change X, open Y"

| I want to understand / change… | File | Entry point |
|--------------------------------|------|-------------|
| GUI starts a run / reads progress | `crates/tracker-app/src/app/state/jobs.rs` | `start_tracking`, `poll_tracking` |
| Background jobs as a state (`Job` enum) | `crates/tracker-app/src/app/state/jobs.rs` | `Job` |
| Review/results state, rep clips | `crates/tracker-app/src/app/state/review.rs` | `SessionResults`, `advance_rep_clip` |
| Seed / calibration / frame position | `crates/tracker-app/src/app/state/session.rs` | `place_seed`, `set_frame` |
| decode + orchestration | `crates/tracker-app/src/tracking.rs` | `run_tracking_worker`, `run_tracking_loop` |
| the search ("where did it go") | `crates/tracker-core/src/tracker.rs` | `TemplateTracker::step` |
| ZNCC similarity metric | `crates/tracker-core/src/metric.rs` | `Zncc::score` |
| trust/veto/gaps/state machine | `crates/tracker-core/src/session.rs` | `TrackingSession::step` |
| motion model / reachability gate | `crates/tracker-core/src/motion.rs` | — |
| the trajectory aggregate | `crates/tracker-core/src/bar_path.rs` | `PathPoint`, `BarPath::new` |
| **velocity maths** | `crates/tracker-core/src/velocity.rs` | `velocity_series` |
| rep segmentation | `crates/tracker-core/src/rep.rs` | `segment_reps` |
| per-rep metrics (mean/peak/loss) | `crates/tracker-core/src/rep_metrics.rs` | — |
| pixel↔metre calibration | `crates/tracker-core/src/calibration.rs` | `px_to_meters` |
| ground-truth grading | `crates/tracker-core/src/accuracy.rs`, `crates/tracker-app/src/grade.rs` | `grade` |
| CSV/JSON export | `crates/tracker-core/src/export.rs` | — |
| overlay video burn-in | `crates/tracker-core/src/overlay.rs`, `crates/tracker-app/src/overlay_export.rs` | — |
| side panel (table/chart/education) | `crates/tracker-app/src/app/side_panel.rs`, `app/results/*` | `show` |
| video panel (path overlay draw) | `crates/tracker-app/src/app/video_panel.rs` | `show` |
| display-frame decode (scrubbing) | `crates/tracker-app/src/decode_worker.rs` | `run_decode_worker` |

---

## 8. Suggested reading order for a new contributor

1. `CONTEXT.md` — learn the words.
2. This file §1–§2 — the mental model + the GUI↔core boundary.
3. `bar_path.rs` — the aggregate everything flows through; small, central.
4. `tracking.rs` `run_tracking_loop` — the cycle that drives everything.
5. `tracker.rs` `TemplateTracker::step` — the actual CV.
6. `session.rs` `TrackingSession::step` — the trust logic + state machine.
7. `velocity.rs` then `rep.rs` — how numbers come out the other end.
8. `docs/architecture.md` — the rules that keep all this decoupled, once the
   shapes are familiar.
