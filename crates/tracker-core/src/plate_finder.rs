//! Whole-frame plate finder.
//!
//! Finds weight plates (seen end-on as circles) anywhere in a frame, with no
//! seed. Unlike `CircleTracker`, which only searches near a known position,
//! this scans the whole image.
//!
//! Algorithm: gradient-directed circular Hough transform.
//!
//! 1. The frame is box-downsampled by an integer factor so its shorter side is
//!    about [`TARGET_SHORT_SIDE`] pixels (keeps the cost per frame small).
//! 2. Edge pixels (Sobel magnitude above a threshold) vote, along their
//!    gradient direction in both senses, for a centre at each candidate radius.
//!    Voting along the gradient is what rejects long straight edges (rack
//!    uprights): a line casts one vote per accumulator cell per radius, far
//!    below the `2*pi*r` votes a real circle collects.
//! 3. The score of a centre is its best radius' vote count (summed over a
//!    3x3 centre neighbourhood) divided by the circumference,
//!    clamped to 1: the fraction of the rim with edge support.
//! 4. Peaks are taken greedily best-first with centre suppression, then each
//!    is refined at full resolution in a small window.
//! 5. The collar (`sleeve_end`) is a second, small-radius Hough run at full
//!    resolution restricted to the inside of the plate.
//!
//! The radius range is a fraction of the frame's shorter side, never pixels,
//! so the finder is resolution independent.

use crate::geometry::{Frame, Point};

/// The coarse stage works on an image whose shorter side is about this.
const TARGET_SHORT_SIDE: u32 = 96;

/// A detected plate.
#[derive(Debug, Clone, PartialEq)]
pub struct PlateCandidate {
    /// Plate centre in full-frame pixels.
    pub center: Point,
    /// Plate radius in full-frame pixels.
    pub radius_px: f64,
    /// Centre of the small inner collar circle, when found. This is the point
    /// the Bar Point maps to.
    pub sleeve_end: Option<Point>,
    /// Rim score: thin radial edge pixels within 1.5 px of the refined circle,
    /// divided by its circumference. About 1 for a complete clean rim, lower
    /// for occluded or weak ones; deliberately not clamped, so it is a
    /// monotone measure of rim evidence comparable across frames.
    pub score: f64,
}

/// Configuration for [`find_plates`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateFinderConfig {
    min_radius_fraction: f64,
    max_radius_fraction: f64,
    edge_threshold: f64,
    min_support: f64,
    min_score: f64,
    max_candidates: usize,
}

impl PlateFinderConfig {
    pub fn builder() -> PlateFinderConfigBuilder {
        PlateFinderConfigBuilder::default()
    }

    pub fn default_config() -> Self {
        PlateFinderConfigBuilder::default().build()
    }

    /// Smallest plate radius, as a fraction of the shorter frame side.
    pub fn min_radius_fraction(&self) -> f64 {
        self.min_radius_fraction
    }
    /// Largest plate radius, as a fraction of the shorter frame side.
    pub fn max_radius_fraction(&self) -> f64 {
        self.max_radius_fraction
    }
    /// Minimum Sobel gradient (luma units per pixel) for an edge pixel.
    pub fn edge_threshold(&self) -> f64 {
        self.edge_threshold
    }
    /// Minimum rim support fraction to accept a candidate.
    pub fn min_support(&self) -> f64 {
        self.min_support
    }
    /// Candidates whose final score is below this are dropped.
    pub fn min_score(&self) -> f64 {
        self.min_score
    }
    /// Maximum number of candidates returned.
    pub fn max_candidates(&self) -> usize {
        self.max_candidates
    }
}

impl Default for PlateFinderConfig {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Builder for [`PlateFinderConfig`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateFinderConfigBuilder {
    min_radius_fraction: f64,
    max_radius_fraction: f64,
    edge_threshold: f64,
    min_support: f64,
    min_score: f64,
    max_candidates: usize,
}

impl Default for PlateFinderConfigBuilder {
    fn default() -> Self {
        Self {
            min_radius_fraction: 0.12,
            max_radius_fraction: 0.30,
            edge_threshold: 4.0,
            min_support: 0.25,
            min_score: 0.4,
            max_candidates: 5,
        }
    }
}

impl PlateFinderConfigBuilder {
    pub fn min_radius_fraction(mut self, v: f64) -> Self {
        self.min_radius_fraction = v;
        self
    }
    pub fn max_radius_fraction(mut self, v: f64) -> Self {
        self.max_radius_fraction = v;
        self
    }
    pub fn edge_threshold(mut self, v: f64) -> Self {
        self.edge_threshold = v;
        self
    }
    pub fn min_support(mut self, v: f64) -> Self {
        self.min_support = v;
        self
    }
    pub fn min_score(mut self, v: f64) -> Self {
        self.min_score = v;
        self
    }
    pub fn max_candidates(mut self, v: usize) -> Self {
        self.max_candidates = v;
        self
    }
    pub fn build(self) -> PlateFinderConfig {
        PlateFinderConfig {
            min_radius_fraction: self.min_radius_fraction,
            max_radius_fraction: self.max_radius_fraction,
            edge_threshold: self.edge_threshold,
            min_support: self.min_support,
            min_score: self.min_score,
            max_candidates: self.max_candidates,
        }
    }
}

/// Single-channel float image.
struct Gray {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Gray {
    fn from_frame(frame: &Frame, scale: usize) -> Self {
        let r = Rect {
            x0: 0,
            y0: 0,
            x1: frame.width() as i64,
            y1: frame.height() as i64,
        };
        Self::from_frame_rect(frame, r, scale).0
    }

    /// Box-downsampled luma of the `rect` crop (clamped to the frame).
    /// Returns the image and the full-resolution origin of its pixel (0, 0).
    fn from_frame_rect(frame: &Frame, rect: Rect, scale: usize) -> (Self, (i64, i64)) {
        let fw = frame.width() as usize;
        let fh = frame.height() as usize;
        let rect = rect.clamp_to(fw, fh);
        let (ox, oy) = (rect.x0 as usize, rect.y0 as usize);
        let w = (rect.w() / scale).max(1);
        let h = (rect.h() / scale).max(1);
        let rgb = frame.rgb();
        let mut data = vec![0f32; w * h];
        let norm = 1.0 / (scale * scale) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0f32;
                for dy in 0..scale {
                    let row = (oy + y * scale + dy).min(fh - 1) * fw;
                    for dx in 0..scale {
                        let i = (row + (ox + x * scale + dx).min(fw - 1)) * 3;
                        sum += 0.299 * rgb[i] as f32
                            + 0.587 * rgb[i + 1] as f32
                            + 0.114 * rgb[i + 2] as f32;
                    }
                }
                data[y * w + x] = sum * norm;
            }
        }
        (Self { w, h, data }, (ox as i64, oy as i64))
    }

    #[inline]
    fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }

    /// Sobel gradient (normalised to luma units per pixel) at an interior pixel.
    #[inline]
    fn sobel(&self, x: usize, y: usize) -> (f32, f32) {
        let a = self.at(x - 1, y - 1);
        let b = self.at(x, y - 1);
        let c = self.at(x + 1, y - 1);
        let d = self.at(x - 1, y);
        let f = self.at(x + 1, y);
        let g = self.at(x - 1, y + 1);
        let h = self.at(x, y + 1);
        let i = self.at(x + 1, y + 1);
        let gx = (c + 2.0 * f + i) - (a + 2.0 * d + g);
        let gy = (g + 2.0 * h + i) - (a + 2.0 * b + c);
        (gx * 0.125, gy * 0.125)
    }
}

/// Integer rectangle `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy)]
struct Rect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Rect {
    fn w(&self) -> usize {
        (self.x1 - self.x0).max(0) as usize
    }
    fn h(&self) -> usize {
        (self.y1 - self.y0).max(0) as usize
    }
    fn clamp_to(self, w: usize, h: usize) -> Rect {
        Rect {
            x0: self.x0.clamp(0, w as i64),
            y0: self.y0.clamp(0, h as i64),
            x1: self.x1.clamp(0, w as i64),
            y1: self.y1.clamp(0, h as i64),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Peak {
    cx: f64,
    cy: f64,
    r: f64,
    support: f64,
}

/// A thin (non-maximum-suppressed) edge pixel with unit gradient direction.
#[derive(Clone, Copy)]
struct EdgePt {
    x: f32,
    y: f32,
    ux: f32,
    uy: f32,
}

/// Thin edge pixels inside `rect` whose Sobel magnitude exceeds `edge_threshold`.
fn edge_points(gray: &Gray, rect: Rect, edge_threshold: f64) -> Vec<EdgePt> {
    let rect = rect.clamp_to(gray.w, gray.h);
    let thr2 = (edge_threshold * edge_threshold) as f32;
    let mut out = Vec::new();
    if gray.w < 3 || gray.h < 3 {
        return out;
    }
    let ex0 = rect.x0.max(1) as usize;
    let ey0 = rect.y0.max(1) as usize;
    let ex1 = (rect.x1 as usize).min(gray.w - 1);
    let ey1 = (rect.y1 as usize).min(gray.h - 1);
    if ex1 <= ex0 || ey1 <= ey0 {
        return out;
    }
    // Gradient + magnitude over the window plus a 1px margin (for NMS).
    let gx0 = ex0.saturating_sub(1).max(1);
    let gy0 = ey0.saturating_sub(1).max(1);
    let gx1 = (ex1 + 1).min(gray.w - 1);
    let gy1 = (ey1 + 1).min(gray.h - 1);
    let gw = gx1.saturating_sub(gx0);
    let gh = gy1.saturating_sub(gy0);
    let mut grad = vec![(0f32, 0f32, 0f32); gw * gh];
    for y in gy0..gy1 {
        for x in gx0..gx1 {
            let (gx, gy) = gray.sobel(x, y);
            grad[(y - gy0) * gw + (x - gx0)] = (gx, gy, gx * gx + gy * gy);
        }
    }
    for y in ey0..ey1 {
        for x in ex0..ex1 {
            let (gx, gy, m2) = grad[(y - gy0) * gw + (x - gx0)];
            if m2 < thr2 {
                continue;
            }
            // Non-maximum suppression along the gradient: keep thin edges only.
            let inv = 1.0 / m2.sqrt();
            let nx = (gx * inv).round() as i64;
            let ny = (gy * inv).round() as i64;
            let mag_at = |xx: i64, yy: i64| -> f32 {
                let lx = xx - gx0 as i64;
                let ly = yy - gy0 as i64;
                if lx < 0 || ly < 0 || lx >= gw as i64 || ly >= gh as i64 {
                    0.0
                } else {
                    grad[ly as usize * gw + lx as usize].2
                }
            };
            if m2 < mag_at(x as i64 + nx, y as i64 + ny)
                || m2 <= mag_at(x as i64 - nx, y as i64 - ny)
            {
                continue;
            }
            out.push(EdgePt {
                x: x as f32,
                y: y as f32,
                ux: gx * inv,
                uy: gy * inv,
            });
        }
    }
    out
}

/// Least-squares circle refinement (Kasa fit) on edge points lying within
/// `band` of the rough circle, with two outlier-trimming passes.
fn refine_circle(pts: &[EdgePt], cx: f64, cy: f64, r: f64, band: f64) -> Option<(f64, f64, f64)> {
    let (mut cx, mut cy, mut r) = (cx, cy, r);
    let mut band = band;
    let mut fitted = None;
    for _ in 0..3 {
        let sel: Vec<(f64, f64)> = pts
            .iter()
            .map(|e| (e.x as f64, e.y as f64))
            .filter(|&(x, y)| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs() <= band)
            .collect();
        if sel.len() < 12 {
            return fitted;
        }
        // Kasa: x^2+y^2 + D x + E y + F = 0, solved via 3x3 normal equations
        // on coordinates centred at the current estimate.
        let (mut sxx, mut sxy, mut syy, mut sx, mut sy, mut sxz, mut syz, mut sz) =
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let n = sel.len() as f64;
        for &(x, y) in &sel {
            let (u, v) = (x - cx, y - cy);
            let z = u * u + v * v;
            sxx += u * u;
            sxy += u * v;
            syy += v * v;
            sx += u;
            sy += v;
            sxz += u * z;
            syz += v * z;
            sz += z;
        }
        // Solve [sxx sxy sx; sxy syy sy; sx sy n] [a b c]^T = [sxz syz sz]^T
        // where centre = (a/2, b/2), r^2 = c + (a^2+b^2)/4.
        let m = [[sxx, sxy, sx], [sxy, syy, sy], [sx, sy, n]];
        let rhs = [sxz, syz, sz];
        let det = |m: &[[f64; 3]; 3]| {
            m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
        };
        let d = det(&m);
        if d.abs() < 1e-9 {
            return fitted;
        }
        let mut sol = [0.0; 3];
        for (k, s) in sol.iter_mut().enumerate() {
            let mut mk = m;
            for i in 0..3 {
                mk[i][k] = rhs[i];
            }
            *s = det(&mk) / d;
        }
        let (a, b, c) = (sol[0], sol[1], sol[2]);
        let r2 = c + (a * a + b * b) / 4.0;
        if r2 <= 0.0 {
            return fitted;
        }
        cx += a / 2.0;
        cy += b / 2.0;
        r = r2.sqrt();
        fitted = Some((cx, cy, r));
        band = (band * 0.6).max(1.5);
    }
    fitted
}

/// Gradient-directed circular Hough. Edge pixels inside `edge_rect` vote for
/// centres inside `center_rect`, for integer radii `r_min..=r_max`. Returns
/// per-centre best peaks with support >= `min_support`, best first, with
/// greedy suppression of nearby centres.
fn hough(
    gray: &Gray,
    edge_rect: Rect,
    center_rect: Rect,
    (r_min, r_max): (usize, usize),
    edge_threshold: f64,
    (min_support, max_peaks): (f64, usize),
    radius_weight: f64,
) -> Vec<Peak> {
    let edge_rect = edge_rect.clamp_to(gray.w, gray.h);
    let cw = center_rect.w();
    let ch = center_rect.h();
    if cw == 0 || ch == 0 || r_max < r_min || gray.w < 3 || gray.h < 3 {
        return Vec::new();
    }
    let nr = r_max - r_min + 1;
    let mut acc = vec![0u16; cw * ch * nr];
    for e in edge_points(gray, edge_rect, edge_threshold) {
        for sign in [1.0f32, -1.0] {
            for ri in 0..nr {
                let r = (r_min + ri) as f32;
                let px = (e.x + sign * e.ux * r).round() as i64 - center_rect.x0;
                let py = (e.y + sign * e.uy * r).round() as i64 - center_rect.y0;
                if px >= 0 && py >= 0 && (px as usize) < cw && (py as usize) < ch {
                    let idx = (ri * ch + py as usize) * cw + px as usize;
                    acc[idx] = acc[idx].saturating_add(1);
                }
            }
        }
    }

    // Per-centre best radius, with 3x3 centre pooling.
    let mut cells: Vec<Peak> = Vec::new();
    for cy in 0..ch {
        for cx in 0..cw {
            let mut best = (0.0f64, 0usize);
            let mut best_key = 0.0f64;
            for ri in 0..nr {
                let mut sum = 0u32;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let x = cx as i64 + dx;
                        let y = cy as i64 + dy;
                        if x >= 0 && y >= 0 && (x as usize) < cw && (y as usize) < ch {
                            sum += acc[(ri * ch + y as usize) * cw + x as usize] as u32;
                        }
                    }
                }
                let r = (r_min + ri) as f64;
                let support = sum as f64 / (2.0 * std::f64::consts::PI * r);
                let k = support * r.powf(radius_weight);
                if k > best_key {
                    best_key = k;
                    best = (support, ri);
                }
            }
            if best.0 >= min_support {
                cells.push(Peak {
                    cx: (center_rect.x0 + cx as i64) as f64,
                    cy: (center_rect.y0 + cy as i64) as f64,
                    r: (r_min + best.1) as f64,
                    support: best.0,
                });
            }
        }
    }
    let key = |p: &Peak| p.support * p.r.powf(radius_weight);
    cells.sort_by(|a, b| key(b).total_cmp(&key(a)));

    let mut out: Vec<Peak> = Vec::new();
    for c in cells {
        if out.len() >= max_peaks {
            break;
        }
        let clash = out.iter().any(|p| {
            let d = ((p.cx - c.cx).powi(2) + (p.cy - c.cy).powi(2)).sqrt();
            d < 0.5 * p.r.max(c.r)
        });
        if !clash {
            out.push(c);
        }
    }
    out
}

/// Rim score of a circle: radial thin edge pixels within 1.5 px of it per
/// unit of circumference.
fn rim_score(pts: &[EdgePt], cx: f64, cy: f64, r: f64) -> f64 {
    if r <= 0.0 {
        return 0.0;
    }
    let n = pts
        .iter()
        .filter(|e| {
            let dx = e.x as f64 - cx;
            let dy = e.y as f64 - cy;
            let d = dx.hypot(dy);
            d > 1e-6
                && (d - r).abs() <= 1.5
                && ((dx * e.ux as f64 + dy * e.uy as f64) / d).abs() >= 0.7
        })
        .count();
    n as f64 / (2.0 * std::f64::consts::PI * r)
}

/// Fit a circle near (`cx`, `cy`, `r`) (full-frame pixels) on a full-resolution
/// crop around it. Returns the refined circle (falling back to the input if the
/// fit is unusable or drifts more than `max_shift`) and its rim score.
fn refine_in_crop(
    frame: &Frame,
    (cx, cy, r): (f64, f64, f64),
    band: f64,
    max_shift: f64,
    thr: f64,
) -> ((f64, f64, f64), f64) {
    let reach = (r + band + max_shift).ceil() as i64 + 2;
    let rect = Rect {
        x0: cx.round() as i64 - reach,
        y0: cy.round() as i64 - reach,
        x1: cx.round() as i64 + reach + 1,
        y1: cy.round() as i64 + reach + 1,
    };
    let (crop, (ox, oy)) = Gray::from_frame_rect(frame, rect, 1);
    let (ox, oy) = (ox as f64, oy as f64);
    let whole = Rect {
        x0: 0,
        y0: 0,
        x1: crop.w as i64,
        y1: crop.h as i64,
    };
    let pts = edge_points(&crop, whole, thr);
    let (lx, ly) = (cx - ox, cy - oy);
    let (fx, fy, fr) = match refine_circle(&pts, lx, ly, r, band) {
        Some((fx, fy, fr))
            if (fx - lx).hypot(fy - ly) <= max_shift && (fr - r).abs() <= max_shift =>
        {
            (fx, fy, fr)
        }
        _ => (lx, ly, r),
    };
    let score = rim_score(&pts, fx, fy, fr);
    ((fx + ox, fy + oy, fr), score)
}

/// Find plates anywhere in `frame`, best (highest rim score) first.
///
/// Pass 1 (outer plate): the frame is box-downsampled by an integer factor so
/// its shorter side is about 96 px, and a gradient-directed circular Hough runs
/// over the configured radius range. Each peak is then refined by a
/// least-squares fit on a full-resolution crop around it (never the whole
/// frame) and scored; candidates below `min_score` are dropped.
///
/// Pass 2 (collar/hub): for each kept plate, a small-radius Hough inside the
/// plate (radius 0.1 to 0.55 of the plate radius) on the same downsampled
/// image, so its cost does not grow with the plate size; the hub centre is
/// refined on a crop and becomes `sleeve_end`.
pub fn find_plates(frame: &Frame, config: &PlateFinderConfig) -> Vec<PlateCandidate> {
    let fw = frame.width() as usize;
    let fh = frame.height() as usize;
    let short = fw.min(fh);
    if short < 16 {
        return Vec::new();
    }
    let scale = (short / TARGET_SHORT_SIDE as usize).max(1);
    let small = Gray::from_frame(frame, scale);
    let s = scale as f64;

    let rmin = ((config.min_radius_fraction * short as f64 / s).round() as usize).max(3);
    let rmax = (config.max_radius_fraction * short as f64 / s).round() as usize;
    let whole = Rect {
        x0: 0,
        y0: 0,
        x1: small.w as i64,
        y1: small.h as i64,
    };
    let coarse = hough(
        &small,
        whole,
        whole,
        (rmin, rmax),
        config.edge_threshold,
        (config.min_support, config.max_candidates),
        0.0,
    );

    let mut out = Vec::new();
    for p in coarse {
        // Coarse pixel i covers full pixels [i*s, (i+1)*s): centre (i+0.5)*s - 0.5.
        let ccx = (p.cx + 0.5) * s - 0.5;
        let ccy = (p.cy + 0.5) * s - 0.5;
        let ((cx, cy, r), score) = refine_in_crop(
            frame,
            (ccx, ccy, p.r * s),
            (s * 2.5).max(2.0),
            s * 3.0,
            config.edge_threshold,
        );
        if score < config.min_score {
            continue;
        }
        let center = Point::new(cx, cy);
        let sleeve_end = find_collar(frame, &small, scale, center, r, config);
        out.push(PlateCandidate {
            center,
            radius_px: r,
            sleeve_end,
            score,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.radius_px.total_cmp(&a.radius_px))
    });
    out
}

/// Search for the collar/hub circle inside a plate, at the coarse scale.
fn find_collar(
    frame: &Frame,
    small: &Gray,
    scale: usize,
    center: Point,
    plate_r: f64,
    config: &PlateFinderConfig,
) -> Option<Point> {
    let s = scale as f64;
    let rc = plate_r / s;
    let r_lo = ((0.10 * rc).round() as usize).max(2);
    let r_hi = (0.55 * rc).round() as usize;
    if r_hi < r_lo + 1 {
        return None;
    }
    let cx = ((center.x + 0.5) / s - 0.5).round() as i64;
    let cy = ((center.y + 0.5) / s - 0.5).round() as i64;
    let slack = ((0.25 * rc).ceil() as i64).max(1);
    let reach = r_hi as i64 + slack + 1;
    let center_rect = Rect {
        x0: cx - slack,
        y0: cy - slack,
        x1: cx + slack + 1,
        y1: cy + slack + 1,
    };
    let edge_rect = Rect {
        x0: cx - reach,
        y0: cy - reach,
        x1: cx + reach + 1,
        y1: cy + reach + 1,
    };
    let peaks = hough(
        small,
        edge_rect,
        center_rect.clamp_to(small.w, small.h),
        (r_lo, r_hi),
        config.edge_threshold,
        (config.min_support, 1),
        // Rank by absolute vote count so the hub beats the tiny axle circle.
        1.0,
    );
    let p = peaks.first()?;
    let (hx, hy) = ((p.cx + 0.5) * s - 0.5, (p.cy + 0.5) * s - 0.5);
    let ((fx, fy, _), _) = refine_in_crop(
        frame,
        (hx, hy, p.r * s),
        (s * 2.0).max(2.0),
        s * 2.0,
        config.edge_threshold,
    );
    Some(Point::new(fx, fy))
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
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                if dx * dx + dy * dy <= r * r {
                    f.set_pixel(x, y, rgb);
                }
            }
        }
    }

    fn dist(p: Point, x: f64, y: f64) -> f64 {
        ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt()
    }

    #[test]
    fn finds_filled_disc_within_two_pixels_of_true_centre() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 140.3, 210.7, 50.0, [20, 40, 200]);
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(!found.is_empty());
        let best = &found[0];
        assert!(dist(best.center, 140.3, 210.7) <= 2.0, "{:?}", best);
        assert!((best.radius_px - 50.0).abs() <= 3.0, "{:?}", best);
        assert!(best.sleeve_end.is_none(), "{:?}", best);
    }

    #[test]
    fn finds_inner_collar_circle_as_sleeve_end() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 150.0, 200.0, 60.0, [20, 40, 200]);
        fill_disc(&mut f, 153.0, 201.0, 9.0, [240, 240, 240]);
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(!found.is_empty());
        let best = &found[0];
        assert!(dist(best.center, 150.0, 200.0) <= 2.0, "{:?}", best);
        let sleeve = best.sleeve_end.expect("collar should be found");
        assert!(dist(sleeve, 153.0, 201.0) <= 2.0, "{:?}", sleeve);
    }

    #[test]
    fn ignores_long_straight_edges_like_rack_uprights() {
        let mut f = blank(300, 400, 90);
        for y in 0..400 {
            for x in 60..100 {
                f.set_pixel(x, y, [220, 220, 220]);
            }
            for x in 200..204 {
                f.set_pixel(x, y, [20, 20, 20]);
            }
        }
        for x in 0..300 {
            for y in 300..306 {
                f.set_pixel(x, y, [250, 250, 250]);
            }
        }
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn two_discs_are_both_returned_best_first() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 90.0, 100.0, 45.0, [20, 40, 200]);
        fill_disc(&mut f, 200.0, 290.0, 45.0, [250, 220, 20]);
        // Occlude part of the second so it has weaker edge support.
        for y in 290..340 {
            for x in 150..250 {
                f.set_pixel(x, y, [90, 90, 90]);
            }
        }
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(found.len() >= 2, "{:?}", found);
        assert!(found[0].score > found[1].score, "{:?}", found);
        assert!(dist(found[0].center, 90.0, 100.0) <= 2.5, "{:?}", found);
        assert!(dist(found[1].center, 200.0, 290.0) <= 6.0, "{:?}", found);
    }

    #[test]
    fn blank_frame_returns_no_candidates() {
        let f = blank(300, 400, 120);
        assert!(find_plates(&f, &PlateFinderConfig::default_config()).is_empty());
    }

    #[test]
    fn radius_range_is_a_fraction_of_the_shorter_side() {
        // Same scene at 2x resolution must give the same result scaled by 2.
        let mut f = blank(600, 800, 90);
        fill_disc(&mut f, 280.0, 420.0, 100.0, [20, 40, 200]);
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(!found.is_empty());
        assert!(dist(found[0].center, 280.0, 420.0) <= 3.0, "{:?}", found[0]);
        assert!((found[0].radius_px - 100.0).abs() <= 5.0);
    }

    #[test]
    fn timing_on_portrait_576x1024() {
        let mut f = blank(576, 1024, 90);
        fill_disc(&mut f, 300.0, 500.0, 90.0, [20, 40, 200]);
        fill_disc(&mut f, 300.0, 500.0, 18.0, [240, 240, 240]);
        for y in 0..1024 {
            for x in 40..70 {
                f.set_pixel(x, y, [200, 200, 200]);
            }
        }
        let t = std::time::Instant::now();
        let n = 5;
        let mut found = Vec::new();
        for _ in 0..n {
            found = find_plates(&f, &PlateFinderConfig::default_config());
        }
        eprintln!("find_plates: {:?}/frame", t.elapsed() / n);
        assert!(!found.is_empty());
        assert!(dist(found[0].center, 300.0, 500.0) <= 3.0, "{:?}", found[0]);
    }

    fn lcg_noise(f: &mut Frame, amp: i32, seed: u64) {
        let mut st = seed;
        let (w, h) = (f.width() as i64, f.height() as i64);
        for y in 0..h {
            for x in 0..w {
                st = st
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let n = ((st >> 33) as i32 % (2 * amp + 1)) - amp;
                let p = f.pixel(x as u32, y as u32).unwrap();
                let q = [
                    (p[0] as i32 + n).clamp(0, 255) as u8,
                    (p[1] as i32 + n).clamp(0, 255) as u8,
                    (p[2] as i32 + n).clamp(0, 255) as u8,
                ];
                f.set_pixel(x, y, q);
            }
        }
    }

    #[test]
    fn noisy_texture_scene_yields_only_the_real_disc() {
        let mut f = blank(300, 400, 120);
        fill_disc(&mut f, 150.0, 200.0, 60.0, [20, 40, 200]);
        for y in 0..400 {
            f.set_pixel(30, y, [250, 250, 250]);
            f.set_pixel(31, y, [250, 250, 250]);
        }
        for x in 0..300 {
            f.set_pixel(x, 350, [10, 10, 10]);
            f.set_pixel(x, 351, [10, 10, 10]);
        }
        lcg_noise(&mut f, 8, 7);
        let found = find_plates(&f, &PlateFinderConfig::builder().min_score(0.4).build());
        assert_eq!(found.len(), 1, "{:?}", found);
        assert!(dist(found[0].center, 150.0, 200.0) <= 2.5, "{:?}", found);
    }

    #[test]
    fn pure_speckle_floor_has_no_candidates_above_min_score() {
        let mut f = blank(300, 400, 120);
        lcg_noise(&mut f, 10, 99);
        let found = find_plates(&f, &PlateFinderConfig::builder().min_score(0.4).build());
        assert!(found.is_empty(), "{:?}", found);
    }

    #[test]
    fn same_scene_at_two_resolutions_scales_radius() {
        for k in [1.0f64, 2.0, 3.0] {
            let (w, h) = ((300.0 * k) as u32, (400.0 * k) as u32);
            let mut f = blank(w, h, 90);
            fill_disc(&mut f, 140.0 * k, 210.0 * k, 50.0 * k, [20, 40, 200]);
            let found = find_plates(&f, &PlateFinderConfig::default_config());
            assert!(!found.is_empty(), "k={k}");
            assert!(
                dist(found[0].center, 140.0 * k, 210.0 * k) <= 1.0 + k,
                "k={k} {:?}",
                found[0]
            );
            assert!(
                (found[0].radius_px - 50.0 * k).abs() <= 2.0 * k + 1.0,
                "k={k} {:?}",
                found[0]
            );
        }
    }

    #[test]
    fn out_of_range_radius_discs_are_not_returned() {
        let mut small = blank(300, 400, 90);
        fill_disc(&mut small, 150.0, 200.0, 18.0, [20, 40, 200]);
        assert!(find_plates(&small, &PlateFinderConfig::default_config()).is_empty());
        let mut big = blank(300, 400, 90);
        fill_disc(&mut big, 150.0, 200.0, 140.0, [20, 40, 200]);
        let found = find_plates(&big, &PlateFinderConfig::default_config());
        assert!(
            found.iter().all(|c| c.radius_px <= 0.30 * 300.0 + 3.0),
            "{:?}",
            found
        );
    }

    #[test]
    fn degenerate_frames_do_not_panic() {
        for (w, h) in [
            (1u32, 1u32),
            (3, 3),
            (15, 200),
            (200, 15),
            (2000, 20),
            (20, 2000),
            (16, 16),
        ] {
            let mut f = blank(w, h, 100);
            fill_disc(
                &mut f,
                w as f64 / 2.0,
                h as f64 / 2.0,
                (w.min(h) as f64) / 3.0,
                [250, 10, 10],
            );
            let _ = find_plates(&f, &PlateFinderConfig::default_config());
        }
    }

    #[test]
    fn disc_partly_off_frame_does_not_panic_and_stays_in_range() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 12.0, 200.0, 60.0, [20, 40, 200]);
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        for c in &found {
            assert!(c.radius_px >= 30.0 && c.radius_px <= 100.0, "{:?}", c);
        }
    }

    #[test]
    fn outer_plate_radius_not_hub_radius() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 150.0, 200.0, 70.0, [20, 40, 200]);
        fill_disc(&mut f, 152.0, 201.0, 30.0, [225, 215, 180]);
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        assert!(!found.is_empty());
        let best = &found[0];
        assert!((best.radius_px - 70.0).abs() <= 4.0, "{:?}", best);
        assert!(dist(best.center, 150.0, 200.0) <= 2.5, "{:?}", best);
        let sleeve = best.sleeve_end.expect("hub should give a sleeve_end");
        assert!(dist(sleeve, 152.0, 201.0) <= 3.0, "{:?}", sleeve);
    }

    #[test]
    fn candidates_below_min_score_are_dropped() {
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 150.0, 200.0, 60.0, [20, 40, 200]);
        let all = find_plates(&f, &PlateFinderConfig::builder().min_score(0.0).build());
        assert!(!all.is_empty());
        let none = find_plates(&f, &PlateFinderConfig::builder().min_score(100.0).build());
        assert!(none.is_empty(), "{:?}", none);
        let cfg = PlateFinderConfig::default_config();
        assert!(cfg.min_score() > 0.0);
    }

    #[test]
    fn scores_are_not_saturated_at_one() {
        // A clean full disc and a half-occluded one must score differently.
        let mut f = blank(300, 400, 90);
        fill_disc(&mut f, 90.0, 100.0, 45.0, [20, 40, 200]);
        fill_disc(&mut f, 200.0, 290.0, 45.0, [250, 220, 20]);
        for y in 290..340 {
            for x in 150..250 {
                f.set_pixel(x, y, [90, 90, 90]);
            }
        }
        let found = find_plates(&f, &PlateFinderConfig::builder().min_score(0.0).build());
        assert!(found.len() >= 2);
        assert!(found[0].score != found[1].score);
    }

    #[test]
    fn timing_on_1080x1920() {
        let mut f = blank(1080, 1920, 90);
        fill_disc(&mut f, 540.0, 900.0, 190.0, [20, 40, 200]);
        fill_disc(&mut f, 540.0, 900.0, 80.0, [225, 215, 180]);
        for y in 0..1920 {
            for x in 80..130 {
                f.set_pixel(x, y, [200, 200, 200]);
            }
        }
        let t = std::time::Instant::now();
        let found = find_plates(&f, &PlateFinderConfig::default_config());
        let el = t.elapsed();
        let bound = if cfg!(debug_assertions) { 1500 } else { 50 };
        eprintln!("1080x1920: {el:?}");
        assert!(el.as_millis() < bound, "{el:?}");
        assert!(!found.is_empty());
        assert!((found[0].radius_px - 190.0).abs() <= 8.0, "{:?}", found[0]);
        let sl = found[0].sleeve_end.expect("hub");
        assert!(dist(sl, 540.0, 900.0) <= 8.0, "{:?}", sl);
    }
}
