# Running image-tracker

This document covers how to run image-tracker as a developer or as an end user,
how to install it, how to hand it to someone else, and a manual test script
used to validate builds by hand before a release.

## Prerequisites

- **Rust (stable)** — only needed if you're building from source. Install via
  [rustup](https://rustup.rs/).
- **`ffmpeg` and `ffprobe` on your `PATH`** — image-tracker shells out to both
  for video decode/encode and metadata probing (see
  [ADR 0001](docs/adr/0001-shell-out-to-ffmpeg.md)). Needed whether you build
  from source or run a downloaded binary.

  ```bash
  # Debian/Ubuntu
  sudo apt install ffmpeg

  # Arch/Manjaro
  sudo pacman -S ffmpeg

  # macOS (Homebrew)
  brew install ffmpeg
  ```

  Verify with `ffmpeg -version` and `ffprobe -version`.

## Run as a developer

Clone the repo, then run from the workspace root:

```bash
git clone https://github.com/amadeu01/image-tracker.git
cd image-tracker
```

### GUI

```bash
cargo run -p tracker-app -- path/to/video.mp4
```

Opens the egui window on the given video, ready for seed placement,
calibration, and tracking (see the manual test script below for the
walkthrough).

The video path is optional (task 10.5): `cargo run -p tracker-app` with no
arguments opens straight to an empty-state window ("Open a video to begin").
Use the **Open video…** toolbar button, the **Ctrl+O** shortcut, or the
`video-path` argument to load one — all three go through the same code path,
so opening a second video mid-session resets the workspace (seed,
calibration, tracking run) exactly the way a fresh launch on that video
would.

#### Auto bar finding in the GUI

When a video opens (and no Seed exists yet), the app searches for the bar in
the background; the window stays responsive.

1. The hint banner reads **"Finding the bar… N%"** with a **Cancel** button.
   Cancelling (or placing a seed by hand) stops the search and returns to
   manual seeding.
2. If it finds a moving plate, the banner shows
   **"Proposed bar · Squat · confidence 87% — <reason>"** and a ring is drawn
   on the frame at the proposed spot, with a label. Nothing is a Seed yet.
   - **Confirm** makes the proposal the Seed (jumping to its frame) and keeps
     the lift type for rep counting. Then calibrate and **Track** as usual.
   - **Not the bar** discards it and arms click-to-seed.
3. If the lift is unknown the banner says so and asks you to confirm only if
   the ring is on the bar. If nothing is found (or the clip length is
   unknown) the banner reads "Could not find the bar automatically — click on
   the bar end to seed manually".

While the search runs, Track and the benchmark are refused. While a track is
running or paused by you, clicking a new seed is refused too; after the
tracker loses the bar (the reseed prompt) a click is accepted as before.

Status: implemented and unit-tested, but the on-screen details (ring position
and size, label legibility, banner layout) have not been checked by eye yet;
step 9 of the manual test script below is the check to do.

### Headless CLI (`track` subcommand)

Runs the same tracking pipeline the GUI drives, without a window, and writes
overlay video + CSV/JSON exports. Useful for scripting or CI. Build/run in
`--release` — debug builds are slow enough to make a full video track
painfully.

```bash
cargo run --release -p tracker-app -- track <video> \
  [--seed-frame N --seed X,Y] --out <dir> \
  [--tracker auto|template|color] \
  [--patch-radius N] [--search-radius N] [--min-score F] \
  [--update-threshold F] [--coast-limit N] \
  [--cal x1,y1,x2,y2 --cal-length-m M]
```

Flags:

| Flag | Required | Meaning |
|------|----------|---------|
| `<video>` | yes | Positional path to the input video. |
| `--seed-frame N` | with `--seed` | Frame index to seed the tracker on. Omit both seed flags for auto mode (below). |
| `--seed X,Y` | with `--seed-frame` | Pixel coordinates of the seed point on that frame. |
| `--out <dir>` | yes | Output directory (created if missing). |
| `--tracker auto\|template\|color` | no | Which tracker to run; `auto` (default) suggests one from the seed patch. |
| `--patch-radius N` | no | Template tracker patch radius override. |
| `--search-radius N` | no | Template tracker search-window radius override. |
| `--min-score F` | no | Minimum correlation score to accept a match. |
| `--update-threshold F` | no | Score above which the tracker refreshes its template. |
| `--coast-limit N` | no | Consecutive misses tolerated before pausing (`NeedsReseed`). |
| `--cal x1,y1,x2,y2` | no | Two calibration pixel points (e.g. both edges of a plate). |
| `--cal-length-m M` | no | Real-world distance between the two `--cal` points, in meters. Both `--cal` and `--cal-length-m` are needed together — if only one is given, calibration is skipped (px units used) rather than the run failing. |

When calibration is given, velocity/rep output is in m/s and meters instead
of px/s and pixels.

Headless runs have no UI to place a new seed if the tracker loses the bar, so
`track` auto-resumes from the last known position on every `NeedsReseed`
pause and counts the reseed events, rather than stopping the run.

Output written to `--out/<video-stem>.{csv,json,overlay.mp4}` and
`--out/<video-stem>.reps.{csv,json}`.

#### Auto mode: no seed

Leave out **both** `--seed-frame` and `--seed` and `track` finds the bar
itself:

```bash
cargo run --release -p tracker-app -- track <video> --out <dir>
```

It samples the clip (every 4th frame), looks for plate-like circles, picks the
moving plate, names the lift, and then tracks from there. Progress goes to
stderr (`pre-pass: 10% (…)`); stdout gets three lines:

```
lift: deadlift (starts at its lowest point and rises 1.32 plate diameters)
seed proposal: frame 0 at (178.3, 664.2), plate 196.2 px, confidence 0.84
UNVERIFIED: seed was auto-proposed; confirm or pass --seed-frame/--seed
```

- `lift:` is `squat`, `deadlift` or `unknown`, with the reason in brackets
  (bench press is never auto-detected). Deadlift reps are segmented
  pull-first, and a rep ends only when the bar rests on the floor.
- `seed proposal:` is the frame, Bar Point (pixels), apparent plate diameter in
  pixels and a 0-1 confidence. Low confidence or `unknown` means: check the
  overlay, or rerun with a manual seed.
- `UNVERIFIED` is always printed: the CLI accepts the proposal without a
  human looking at it. Check `<stem>.overlay.mp4` before trusting the numbers.
- If no moving plate is found, the command exits with status 1 and
  `error: no bar could be found automatically (no moving plate-like circle);
  pass --seed-frame and --seed to track manually`. A seed given on only one of
  the two flags is a usage error (exit 2).
- A seed you give always wins: no pre-pass runs, no sidecar is written, and a
  stale `<stem>.run.json` from an earlier auto run of the same stem is
  removed.

Auto runs also write `--out/<stem>.run.json` (and print
`run info (UNVERIFIED) -> <path>`):

| Field | Meaning |
|-------|---------|
| `lift` | `deadlift`, `squat`, `bench` or `unknown` |
| `lift_reason` | why the classifier said so (also the reason for `unknown`) |
| `verified` | `false` for CLI auto runs (nobody confirmed the proposal) |
| `seed_frame` | frame the proposal was made on and tracking started from |
| `seed` | `[x, y]` Bar Point in pixels |
| `plate_px` | apparent plate diameter in pixels, from the detections |

The export `<stem>.json` is unchanged (still a top-level array); `grade` reads
the lift from this sidecar.

Known limit: tracking runs forward from the proposed frame, so a proposal made
late in the clip drops the reps before it. The thresholds behind auto mode
were tuned on four clips and have not yet been checked against hand labels
(see [docs/architecture.md section 6b](docs/architecture.md)).

#### Grading a run: `grade`

```bash
cargo run -p tracker-app -- grade <export.csv> [<truth.csv>] \
  [--plate-px N] [--tolerance-dia F] [--manifest groundtruth/clips.json --clip v5]
```

Without `--manifest` it prints the positional accuracy against the truth CSV
(`--plate-px` default 134, `--tolerance-dia` default 0.1).

With `--manifest` and `--clip` (both, or neither) it also reads the clip's
entry in `clips.json`: `plate_px` becomes the plate diameter (an explicit
`--plate-px` overrides it; a missing or non-positive `plate_px` is an error,
there is no default), the truth CSV defaults to `groundtruth/<clip>.csv` next
to the manifest, and two run checks are printed:

| Check | PASS | MISS | UNKNOWN |
|-------|------|------|---------|
| `LIFT TYPE` | the run's `<stem>.run.json` lift equals the manifest `lift` | a different lift, or the run said `unknown` | no sidecar (e.g. a manual run) or it is unreadable |
| `REP COUNT` | `<stem>.reps.json` has exactly the manifest `reps` | a different count | manifest `reps` is `null` (owner has not counted yet) or no `.reps.json` |

UNKNOWN means "cannot be judged", not "wrong". The manifest is filled in by
hand; see [groundtruth/README.md](groundtruth/README.md).

### Tests

```bash
cargo test
```

Runs the full workspace (`tracker-core` + `tracker-app` + doc-tests).

### Logging

Both the GUI and CLI print a startup banner (`tracker-app <version>
(<os>/<arch>); ffmpeg: <version>`) and the path they're logging to. Set
`RUST_LOG` to control verbosity (defaults are reasonable for normal use;
`RUST_LOG=debug` or `RUST_LOG=tracker_app=trace` for more detail):

```bash
RUST_LOG=debug cargo run -p tracker-app -- path/to/video.mp4
```

Logs are written to a rotating file under your OS's standard data directory
(via the `directories` crate) plus a `logs` subfolder — printed on startup as
`logging to <path>` — in addition to console output.

## Run as a user (no Rust toolchain)

1. Go to the project's [GitHub Releases](https://github.com/amadeu01/image-tracker/releases)
   page and download the tarball for your platform (Linux or macOS).
2. Extract it:

   ```bash
   tar xzf tracker-app-<version>-<platform>.tar.gz
   ```
3. Run the extracted binary directly:

   ```bash
   ./tracker-app path/to/video.mp4
   ```

   Make sure `ffmpeg`/`ffprobe` are installed and on `PATH` (see
   Prerequisites above) — the binary itself doesn't bundle them.

**macOS note**: the binary is not code-signed/notarized. Gatekeeper will
refuse to open it with a plain double-click or `./tracker-app`, saying it's
from an "unidentified developer" / damaged. Either:

- Right-click (or Control-click) the binary in Finder → **Open** → confirm in
  the dialog that appears (only needs to be done once), or
- Clear the quarantine attribute from the terminal:

  ```bash
  xattr -d com.apple.quarantine ./tracker-app
  ```

## Install on your machine

From a source checkout:

```bash
cargo install --path crates/tracker-app
```

Installs a `tracker-app` binary to `~/.cargo/bin` (or wherever
`CARGO_INSTALL_ROOT`/`cargo install` is configured to place binaries). Make
sure that directory is on your `PATH` — `rustup`-managed installs usually add
it automatically. After that:

```bash
tracker-app path/to/video.mp4
tracker-app track <video> --seed-frame N --seed X,Y --out <dir>
```

### Desktop integration (Linux)

`cargo install`/a downloaded binary puts `tracker-app` on your `PATH`, but
Linux desktop launchers (GNOME, KDE, etc.) don't know about it until you
register a `.desktop` file — without one the app shows up as "unknown" with
no icon if it's ever launched from outside a terminal. From a source
checkout:

```bash
mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/128x128/apps
cp assets/image-tracker.desktop ~/.local/share/applications/
cp assets/icons/image-tracker.png ~/.local/share/icons/hicolor/128x128/apps/
update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
```

After that, "Image Tracker" shows up in your application launcher with its
icon, and (on file managers/portals that respect `MimeType=`) as an "Open
With" option for video files. `Exec=tracker-app %f` passes the picked file
straight through as the CLI video-path argument; launching with no file
still works — it opens the empty-state window described above.

## Give it to others

- **Easiest**: point them at the [Releases](https://github.com/amadeu01/image-tracker/releases)
  page and the "Run as a user" instructions above.
- **From source**: point them at this file's "Run as a developer" or "Install
  on your machine" sections — they'll need Rust and `ffmpeg`/`ffprobe`.

## Manual test script

Use this checklist to validate a build by hand (e.g. before cutting a
release, or after a GUI change). Each step lists the action and the expected
result.

1. **Open a video** — `cargo run -p tracker-app -- path/to/video.mp4` (or
   launch the installed/downloaded binary the same way).
   *Expected*: the window opens showing the first frame, and the right-hand
   side panel shows a 5-step usage guide with step 1 done and step 2 ("Place
   Seed") highlighted as current.
2. **Scrub the timeline** — drag the frame slider/scrubber.
   *Expected*: the displayed frame updates to match the scrub position, and
   status in the side panel reflects the current frame.
3. **Place a seed** — press `S` (or click "Place Seed"), then click the bar
   in the frame.
   *Expected*: a crosshair marker appears at the click point; the side
   panel's status section shows the seed position, and a suggested tracker
   type (template/color) appears; the guide advances to step 3 ("Calibrate").
4. **Calibrate** — press `C` (or click "Calibrate"), enter a known length in
   meters (e.g. `0.45` for a competition plate), then click both edges of a
   plate in the frame.
   *Expected*: after the second click, the status section shows a
   pixels-per-meter (or similar) calibration value.
5. **Track** — click "Track".
   *Expected*: a live crosshair follows the bar frame by frame; the side
   panel's events list appends entries (tracking started, any reseed
   pauses/resumes); the guide advances to step 4 while running, then step 5
   ("Review") once done.
6. **Occlusion / reseed** — if the bar is lost for long enough (or use a clip
   known to lose tracking), tracking pauses.
   *Expected*: status turns to "paused" (yellow), a "click a new seed"
   prompt appears; clicking a new seed point on the paused frame + "Resume"
   continues tracking and the events list logs the pause/resume.
7. **CLI track** — run the headless command from a terminal, e.g.:

   ```bash
   cargo run --release -p tracker-app -- track path/to/video.mp4 \
     --seed-frame 300 --seed 260,120 --out /tmp/track-out
   ```

   *Expected*: exits 0; `/tmp/track-out/` contains `<stem>.overlay.mp4`,
   `<stem>.csv`, `<stem>.json`, `<stem>.reps.csv`, and `<stem>.reps.json`;
   console output summarizes points/gaps/reseed events and any detected reps.
8. **Smoke kit** — from the repo root:

   ```bash
   ./scripts/smoke-report.sh
   ```

   *Expected*: builds the release binary, runs the scriptable checks (usage
   exit code, a headless track run producing a CSV with >= 2500 rows,
   version banner + log file presence), and writes
   `docs/smoke/YYYY-MM-DD.md` with those results auto-filled and empty GUI
   checkboxes for steps 1–6 above. Walk through the GUI checklist by hand,
   check off the boxes, fill in tester/platform/notes, and commit the
   report. See [docs/smoke/README.md](docs/smoke/README.md) and
   [docs/smoke/](docs/smoke/) for prior runs.
9. **Auto bar finding (GUI)** — open a clip of a squat or deadlift (not yet
   seeded) and watch.
   *Expected*: the banner shows "Finding the bar… N%" with Cancel; then
   "Proposed bar · <lift> · confidence N%" with a ring on the plate, plus
   Confirm / Not the bar. Look at it: is the ring on the moving plate, is the
   label readable, does the banner fit on one row? Confirm, calibrate, Track:
   the reps should be counted pull-first for a deadlift. Also try Cancel
   mid-search and "Not the bar" (click-to-seed should arm). *These GUI details
   have not been verified by eye yet; record what you see.*
10. **CLI auto mode** — run `track` with no seed:

    ```bash
    cargo run --release -p tracker-app -- track path/to/video.mp4 --out /tmp/auto-out
    ```

    *Expected*: prints `lift:`, `seed proposal:` and `UNVERIFIED`; exits 0;
    `/tmp/auto-out/` has the usual files plus `<stem>.run.json`. On a clip with
    no moving plate it exits 1 with "no bar could be found automatically".
