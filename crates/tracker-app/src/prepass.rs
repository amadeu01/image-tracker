//! Unseeded pre-pass: sample a clip at a stride, find plate-like circles in
//! each sampled frame and ask `tracker_core::propose_seed` which moving
//! plate to track. Pure orchestration over a `FrameDecoder`; no IO of its own.

use std::fmt::Display;

use tracker_core::{
    find_plates, propose_seed, Frame, FrameDetections, PlateFinderConfig, SeedProposal,
    SeedProposalConfig,
};

use crate::frame_cache::FrameDecoder;

/// Default sampling stride, in frames.
///
/// At 30 fps a stride of 4 gives ~7.5 samples per second: dense enough that a
/// plate moves only a fraction of its diameter between samples (so tracks
/// link), while decoding a quarter of the frames keeps the pre-pass cheap.
pub const DEFAULT_PREPASS_STRIDE: u64 = 4;

/// Run the pre-pass over `frame_count` frames, decoding every `stride`-th one
/// (0, stride, 2*stride, ... < frame_count).
///
/// A frame that fails to decode is skipped (nothing recorded for it); the
/// pre-pass only fails when `stride` is 0 or when every sampled decode fails.
/// Frames where no plate is found are still recorded with an empty candidate
/// list so the proposer sees the sample gap. `on_progress` receives
/// `(samples_done, samples_total)` after each sample.
pub fn run_prepass<D>(
    decoder: &mut D,
    frame_count: u64,
    stride: u64,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<Option<SeedProposal>, String>
where
    D: FrameDecoder<Frame = Frame>,
    D::Error: Display,
{
    if stride == 0 {
        return Err("pre-pass stride must be at least 1".to_string());
    }
    let total = frame_count.div_ceil(stride);
    let finder = PlateFinderConfig::default_config();
    let mut detections = Vec::new();
    let mut last_error = None;

    for sample in 0..total {
        let index = sample * stride;
        match decoder.decode_frame(index) {
            Ok(frame) => detections.push(FrameDetections {
                frame_index: index,
                candidates: find_plates(&frame, &finder),
            }),
            Err(e) => {
                tracing::warn!(video_frame_index = index, "pre-pass: skipping frame: {e}");
                last_error = Some(format!("frame {index}: {e}"));
            }
        }
        on_progress(sample + 1, total);
    }

    if total > 0 && detections.is_empty() {
        return Err(format!(
            "pre-pass could not decode any of {total} sampled frames (last error: {})",
            last_error.unwrap_or_default()
        ));
    }
    Ok(propose_seed(
        &detections,
        &SeedProposalConfig::default_config(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32, v: u8) -> Frame {
        Frame::new(w, h, vec![v; (w * h * 3) as usize]).unwrap()
    }

    fn fill_disc(f: &mut Frame, cx: f64, cy: f64, r: f64, rgb: [u8; 3]) {
        for y in 0..f.height() as i64 {
            for x in 0..f.width() as i64 {
                let (dx, dy) = (x as f64 - cx, y as f64 - cy);
                if dx * dx + dy * dy <= r * r {
                    f.set_pixel(x, y, rgb);
                }
            }
        }
    }

    /// Scripted decoder: frame at index i is produced by `make(i)`; indices in
    /// `fail` return an error. Records every requested index.
    struct Scripted {
        requested: Vec<u64>,
        fail: Vec<u64>,
        fail_all: bool,
        make: Box<dyn Fn(u64) -> Frame>,
    }

    impl Scripted {
        fn new(make: impl Fn(u64) -> Frame + 'static) -> Self {
            Self {
                requested: Vec::new(),
                fail: Vec::new(),
                fail_all: false,
                make: Box::new(make),
            }
        }
    }

    impl FrameDecoder for Scripted {
        type Error = String;
        type Frame = Frame;
        fn decode_frame(&mut self, index: u64) -> Result<Frame, String> {
            self.requested.push(index);
            if self.fail_all || self.fail.contains(&index) {
                return Err(format!("boom {index}"));
            }
            Ok((self.make)(index))
        }
    }

    const R: f64 = 40.0;
    const COUNT: u64 = 60;

    /// Disc rising from y=300 to y=220 (one plate diameter) over the clip.
    fn disc_y(i: u64) -> f64 {
        300.0 - 80.0 * (i as f64 / (COUNT - 1) as f64)
    }

    fn moving_disc(i: u64) -> Frame {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 150.0, disc_y(i), R, [20, 40, 200]);
        f
    }

    #[test]
    fn prepass_samples_every_stride_th_frame_and_reports_progress() {
        let mut d = Scripted::new(|_| blank(60, 60, 90));
        let mut progress = Vec::new();
        run_prepass(&mut d, 10, 3, |a, b| progress.push((a, b))).unwrap();
        assert_eq!(d.requested, vec![0, 3, 6, 9]);
        assert_eq!(progress.last(), Some(&(4, 4)));
        assert_eq!(progress.len(), 4);
    }

    #[test]
    fn prepass_returns_none_for_a_clip_with_no_plate() {
        let mut d = Scripted::new(|_| blank(300, 400, 90));
        assert_eq!(run_prepass(&mut d, COUNT, 4, |_, _| {}), Ok(None));
    }

    #[test]
    fn prepass_zero_stride_is_an_error() {
        let mut d = Scripted::new(|_| blank(60, 60, 90));
        assert!(run_prepass(&mut d, 10, 0, |_, _| {}).is_err());
        assert!(d.requested.is_empty());
    }

    #[test]
    fn prepass_skips_a_failing_frame_and_still_proposes() {
        let mut d = Scripted::new(moving_disc);
        d.fail = vec![8, 28];
        let p = run_prepass(&mut d, COUNT, 4, |_, _| {}).unwrap();
        assert!(p.is_some());
    }

    #[test]
    fn prepass_errors_when_every_decode_fails() {
        let mut d = Scripted::new(moving_disc);
        d.fail_all = true;
        let err = run_prepass(&mut d, COUNT, 4, |_, _| {}).unwrap_err();
        assert!(err.contains("could not decode"), "{err}");
    }

    #[test]
    fn prepass_proposes_a_moving_disc() {
        let mut d = Scripted::new(moving_disc);
        let p = run_prepass(&mut d, COUNT, 4, |_, _| {})
            .unwrap()
            .expect("moving disc should be proposed");
        let (ex, ey) = (150.0, disc_y(p.frame_index));
        let dist = ((p.position.x - ex).powi(2) + (p.position.y - ey).powi(2)).sqrt();
        assert!(dist <= 6.0, "{p:?} expected near ({ex},{ey})");
        assert!((p.plate_diameter_px - 2.0 * R).abs() <= 6.0, "{p:?}");
    }

    /// Evidence run on a real clip: `PREPASS_VIDEO=/path cargo test --release
    /// prepass_on_real_clip -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn prepass_on_real_clip_prints_proposal() {
        use crate::seek_source::SeekingFrameDecoder;
        let Ok(path) = std::env::var("PREPASS_VIDEO") else {
            println!("PREPASS_VIDEO not set; skipping");
            return;
        };
        let path = std::path::PathBuf::from(path);
        let meta = crate::ffprobe::probe(&path).expect("probe");
        let count = meta.frame_count.unwrap_or(300).max(1);
        let mut dec = SeekingFrameDecoder::new(
            path,
            meta.display_width(),
            meta.display_height(),
            meta.fps_num,
            meta.fps_den,
        );
        let start = std::time::Instant::now();
        let mut samples = 0;
        let r = run_prepass(&mut dec, count, DEFAULT_PREPASS_STRIDE, |done, _| {
            samples = done
        });
        println!(
            "elapsed_s={:.1} samples={samples} frame_count={count}",
            start.elapsed().as_secs_f64()
        );
        match r {
            Ok(Some(p)) => println!(
                "proposal frame_index={} position=({:.1},{:.1}) plate_diameter_px={:.1} lift={:?} reason={:?} confidence={:.2}",
                p.frame_index, p.position.x, p.position.y, p.plate_diameter_px,
                p.lift.lift, p.lift.reason, p.confidence
            ),
            Ok(None) => println!("proposal: None"),
            Err(e) => println!("prepass error: {e}"),
        }
    }
}
