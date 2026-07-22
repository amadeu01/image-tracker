# GUI threading & responsiveness — rules and audit

**Audience:** every human and LLM that touches `crates/tracker-app`'s GUI
(`src/app/**`, `seek_source.rs`, anything reachable from `eframe::App::update`).
Read the **Cardinal rule** and the **Checklist** before adding any feature that
touches video, files, subprocesses, or the network.

Status: living document. First written 2026-07-22 after users repeatedly hit
the OS "application is not responding — Wait / Force Quit" dialog. The audit
that prompted it is in [§ Audit 2026-07-22](#audit-2026-07-22).

---

## The cardinal rule

> **`eframe::App::update` runs on the main/UI thread and is called once per
> frame. It must never block. Any operation that can take more than ~1–2 ms —
> a subprocess, a decode, a file dialog, a disk read of unknown size, a
> blocking channel `recv()`, a `Child::wait()` — belongs on a background
> thread. `update` may only (a) read already-computed data and (b) call
> `ctx.request_repaint()` to be woken when more arrives.**

Why this is not optional:

- eframe drives the winit event loop on the main thread; on macOS the windowing
  and GPU surface *must* be driven from the main thread. When `update` blocks,
  the event loop stops pumping OS events, and after a short timeout the OS
  (both GNOME/Wayland on Linux and the macOS window server) declares the app
  unresponsive and offers to kill it. This is exactly the dialog users saw.
- "It's usually fast" is not safe. `ffmpeg -ss` seek+decode is tens to hundreds
  of milliseconds and varies with codec, seek distance, and disk. At 60 fps the
  frame budget is **16 ms**. One synchronous decode blows the budget by 10×.

## The model this app should follow

```
main / UI thread (eframe update)          background threads
─────────────────────────────────         ───────────────────────────────
- draw widgets from in-memory state        - decode frames (ffmpeg -ss)
- read latest decoded texture              - run tracking (ffmpeg pipe + Tracker)
- drain channels (try_recv, non-blocking)  - render/encode overlay (ffmpeg sink)
- request_repaint() while work is live     - probe metadata (ffprobe)
- send *requests* to workers (channel)     - file dialogs (rfd Async*)
                                    │
                    channels (Sender/Receiver)
        UI → worker: "decode frame N", "start tracking", "stop"
        worker → UI: "here is frame N", Progress, Done, Error
```

Rules that fall out of the model:

1. **Never call a `FrameDecoder`/`ffmpeg`/`ffprobe` synchronously inside
   `update` or any `*_panel::show`/paint closure.** Request the frame from a
   decode worker and draw the last one you have until the new one arrives.
2. **Channels are drained with `try_recv` in a loop, never `recv`.** A blocking
   `recv` on the UI thread is the same bug as a blocking subprocess. (The
   headless CLI in `cli.rs` *may* block on `recv` — it has no UI thread. Do not
   copy that pattern into `app/`.)
3. **While any worker is live, call `ctx.request_repaint()`** so progress keeps
   flowing (already done in `mod.rs::update` for tracking/export/benchmark).
   When idle, do *not* spin — let egui sleep and repaint on input/worker wake.
4. **File dialogs and pickers go through `rfd`'s async API**
   (`AsyncFileDialog`), or a dedicated thread, so the modal does not freeze the
   render loop.
5. **`Drop` must tear down child processes** (`kill()` + `wait()`), so an
   early stop never leaks an ffmpeg. (See the 2026-07-22 deadlock fix in
   `tracking.rs`: reaping a *still-running* child with an undrained stdout pipe
   deadlocks — only reap after a real EOF; otherwise let `Drop` kill it.)
6. **Keep `tracker-core` out of all of this.** Threading, processes, and egui
   live only in `tracker-app` (CONTEXT.md / ADR-0001 layering). The core stays
   a pure, synchronous, dependency-free library that a worker thread calls.

## Checklist — before you merge a GUI change

- [ ] Does any code path reachable from `update` / a `show` fn / a paint
      closure call `ffmpeg`, `ffprobe`, `Command`, `Child::wait`,
      `read_to_string` on an unbounded file, `pick_file`, or a blocking
      `recv`? → move it to a worker thread.
- [ ] Does it decode a video frame to draw it? → the frame must come from a
      decode worker via a channel/cache populated off-thread, not a synchronous
      `decode_frame`.
- [ ] Does it spin `request_repaint()` every frame even when nothing is
      running? → gate the repaint on "a worker is live" or a specific animation.
- [ ] Manual smoke on Linux **and** macOS: open a video, scrub fast across the
      whole bar, run a full track to the end, hit Finish mid-run — the window
      must stay draggable and repaint throughout. If the OS "not responding"
      dialog appears even once, the change is not done.
- [ ] Every spawned `Child` has a `Drop` that kills+reaps it.

## Audit 2026-07-22

Trigger: users frequently got the OS "application is not responding" dialog,
most reliably during a long tracking run and while scrubbing.

**Root cause — display-frame decoding is synchronous on the UI thread.**
`TrackerApp::ensure_texture` (`app/mod.rs`) runs every `update` and calls
`FrameCache::get(current_frame)`. On a cache miss that calls
`SeekingFrameDecoder::decode_frame` (`seek_source.rs`), which does:

```rust
let mut child = Command::new("ffmpeg").arg("-ss")…​.spawn()?;   // seek_source.rs:103
…
let status = child.wait()?;                                     // seek_source.rs:131
```

— a full ffmpeg process spawn + input-side seek + single-frame decode, **on the
eframe main thread**, blocking the winit event loop for the duration.

Why it fires constantly rather than rarely: during tracking, `poll_tracking`
advances `current_frame` to follow the worker's progress. The LRU cache holds
16 frames and progress is forward-only, so nearly every displayed frame is a
miss → one synchronous ffmpeg spawn **per rendered frame**. The event loop
starves; the OS declares the app hung. The same happens when the user scrubs to
any frame not already cached.

### Findings

| # | Where | Problem | Severity |
|---|-------|---------|----------|
| G1 | `seek_source.rs:103-131` via `ensure_texture` (`mod.rs:263`) | ffmpeg spawn + `wait()` on the UI thread, once per uncached display frame; guaranteed miss-per-frame during tracking | Critical |
| G2 | `video_panel.rs:46` `cache.get(seed.frame_index)` | same synchronous decode inside the paint path when the seed frame isn't cached | High |
| G3 | `app/mod.rs:218` `rfd::FileDialog::pick_file()` | native modal blocks the UI thread while open (log shows multi-second stalls between "dialog opened" and "path picked") | Medium |
| G4 | `ffprobe.rs:191` `.output()` on `open_video` | one synchronous ffprobe per open; brief but still on the UI thread | Low |

### What we did right

- Tracking, thumbnail decode, overlay export, and the strategy benchmark all
  run on dedicated `std::thread`s and communicate via `mpsc` channels that the
  UI drains with `try_recv`. That pattern is correct — the display decoder is
  the one place it was not applied.
- `request_repaint()` is already gated on "a worker is live" for
  tracking/export/benchmark.
- Child processes are killed/reaped on `Drop`.

### Fix plan (proposed milestone 18 — GUI responsiveness)

1. **18.1 Async frame-decode worker.** Replace the synchronous
   `ensure_texture` → `FrameCache::get` path with a decode worker thread that
   owns the `SeekingFrameDecoder`. The UI sends "want frame N" (coalesced to
   the latest request), the worker decodes and returns the `Frame`; `update`
   draws the most recent decoded texture and shows a subtle "decoding…" state
   for a not-yet-arrived frame. Keep the LRU cache, but populate it off-thread.
   The tracking worker already streams positions; the display can follow at
   whatever rate the decoder sustains without ever blocking the UI. *(M)*
2. **18.2 Off-thread `open_video`.** Use `rfd::AsyncFileDialog` (or a picker
   thread) and move the `ffprobe` probe onto the same worker, so opening a
   video never stalls the render loop. Surface "probing…" in the status bar.
   *(S)*
3. **18.3 Responsiveness smoke + guard.** A manual smoke step (scrub-storm +
   full track + Finish-mid-run on Linux and macOS) added to
   `scripts/smoke-report.sh` / `RUNNING.md`, asserting the window stays live;
   this doc's checklist linked from CONTRIBUTING. *(S)*

None of these touch `tracker-core`. 18.1 is the one that removes the freeze;
18.2/18.3 are polish and prevention.

## References (egui / eframe best practice)

- eframe `App::update` is the per-frame main-thread callback; the crate docs
  and examples are explicit that long work must be offloaded to threads and
  communicated back — see the `eframe` docs and the official
  `egui` "download/async" and "background thread" examples
  (`egui_demo_app` / `eframe_template`'s worker patterns).
- `egui::Context::request_repaint` / `request_repaint_after` — the supported
  way for a worker to ask the UI to wake without the UI busy-spinning.
- `rfd::AsyncFileDialog` — non-blocking native file dialogs, the intended
  replacement for `FileDialog::pick_file()` in a live render loop.
- macOS specifically requires window/GPU work on the main thread, which is why
  a blocked `update` is fatal there, not merely janky.

When in doubt, prefer the pattern already used by `tracking.rs` /
`thumbnail_worker.rs` in this repo: a `spawn` that returns a handle holding the
channels, drained non-blocking from `update`.
