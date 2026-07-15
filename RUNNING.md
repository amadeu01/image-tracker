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

### Headless CLI (`track` subcommand)

Runs the same tracking pipeline the GUI drives, without a window, and writes
overlay video + CSV/JSON exports. Useful for scripting or CI. Build/run in
`--release` — debug builds are slow enough to make a full video track
painfully.

```bash
cargo run --release -p tracker-app -- track <video> \
  --seed-frame N --seed X,Y --out <dir> \
  [--tracker auto|template|color] \
  [--patch-radius N] [--search-radius N] [--min-score F] \
  [--update-threshold F] [--coast-limit N] \
  [--cal x1,y1,x2,y2 --cal-length-m M]
```

Flags:

| Flag | Required | Meaning |
|------|----------|---------|
| `<video>` | yes | Positional path to the input video. |
| `--seed-frame N` | yes | Frame index to seed the tracker on. |
| `--seed X,Y` | yes | Pixel coordinates of the seed point on that frame. |
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
