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
    /// Rim edge support in `[0, 1]`.
    pub score: f64,
}

/// Configuration for [`find_plates`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateFinderConfig {
    min_radius_fraction: f64,
    max_radius_fraction: f64,
    edge_threshold: f64,
    min_support: f64,
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
    max_candidates: usize,
}

impl Default for PlateFinderConfigBuilder {
    fn default() -> Self {
        Self {
            min_radius_fraction: 0.05,
            max_radius_fraction: 0.30,
            edge_threshold: 8.0,
            min_support: 0.45,
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
        let fw = frame.width() as usize;
        let fh = frame.height() as usize;
        let w = (fw / scale).max(1);
        let h = (fh / scale).max(1);
        let rgb = frame.rgb();
        let mut data = vec![0f32; w * h];
        let norm = 1.0 / (scale * scale) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0f32;
                for dy in 0..scale {
                    let row = (y * scale + dy).min(fh - 1) * fw;
                    for dx in 0..scale {
                        let i = (row + (x * scale + dx).min(fw - 1)) * 3;
                        sum += 0.299 * rgb[i] as f32
                            + 0.587 * rgb[i + 1] as f32
                            + 0.114 * rgb[i + 2] as f32;
                    }
                }
                data[y * w + x] = sum * norm;
            }
        }
        Self { w, h, data }
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
fn refine_circle(
    gray: &Gray,
    cx: f64,
    cy: f64,
    r: f64,
    band: f64,
    thr: f64,
) -> Option<(f64, f64, f64)> {
    let reach = (r + band).ceil() as i64 + 1;
    let rect = Rect {
        x0: cx.round() as i64 - reach,
        y0: cy.round() as i64 - reach,
        x1: cx.round() as i64 + reach + 1,
        y1: cy.round() as i64 + reach + 1,
    };
    let pts = edge_points(gray, rect, thr);
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
    min_support: f64,
    max_peaks: usize,
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
            for ri in 0..nr {
                let mut sum = 0u32;
                let rj = ri;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let x = cx as i64 + dx;
                        let y = cy as i64 + dy;
                        if x >= 0 && y >= 0 && (x as usize) < cw && (y as usize) < ch {
                            sum += acc[(rj * ch + y as usize) * cw + x as usize] as u32;
                        }
                    }
                }
                let r = (r_min + ri) as f64;
                let support = (sum as f64 / (2.0 * std::f64::consts::PI * r)).min(1.0);
                if support > best.0 {
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
    cells.sort_by(|a, b| b.support.total_cmp(&a.support));

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

/// Find plates anywhere in `frame`, best (highest rim support) first.
///
/// Internally the frame is box-downsampled by an integer factor so the shorter
/// side is about 96 px for the coarse search; centres and radii are then
/// refined and the collar searched at full resolution.
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
        config.min_support,
        config.max_candidates,
    );
    if coarse.is_empty() {
        return Vec::new();
    }

    let full = if scale == 1 {
        None
    } else {
        Some(Gray::from_frame(frame, 1))
    };
    let full_ref = full.as_ref().unwrap_or(&small);

    let mut out = Vec::new();
    for p in coarse {
        // Coarse pixel (cx) covers full pixels [cx*s, (cx+1)*s): centre at (cx+0.5)*s - 0.5.
        let ccx = (p.cx + 0.5) * s - 0.5;
        let ccy = (p.cy + 0.5) * s - 0.5;
        let cr = p.r * s;
        let (center, radius) = match refine_circle(
            full_ref,
            ccx,
            ccy,
            cr,
            (s * 2.5).max(2.0),
            config.edge_threshold,
        ) {
            Some((fx, fy, fr))
                if (fx - ccx).hypot(fy - ccy) <= s * 3.0 && (fr - cr).abs() <= s * 3.0 =>
            {
                (Point::new(fx, fy), fr)
            }
            _ => (Point::new(ccx, ccy), cr),
        };
        let score = p.support;

        let sleeve_end = find_collar(full_ref, center, radius, config);
        out.push(PlateCandidate {
            center,
            radius_px: radius,
            sleeve_end,
            score,
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Search for a small bright/dark collar circle at the plate centre.
fn find_collar(
    gray: &Gray,
    center: Point,
    plate_r: f64,
    config: &PlateFinderConfig,
) -> Option<Point> {
    let r_lo = (0.06 * plate_r).round().max(3.0) as usize;
    let r_hi = (0.35 * plate_r).round() as usize;
    if r_hi < r_lo {
        return None;
    }
    let cx = center.x.round() as i64;
    let cy = center.y.round() as i64;
    let slack = (0.25 * plate_r).ceil() as i64;
    let reach = r_hi as i64 + slack + 2;
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
        gray,
        edge_rect,
        center_rect,
        (r_lo, r_hi),
        config.edge_threshold,
        config.min_support,
        1,
    );
    let p = peaks.first()?;
    match refine_circle(gray, p.cx, p.cy, p.r, 2.5, config.edge_threshold) {
        Some((fx, fy, _)) if (fx - p.cx).hypot(fy - p.cy) <= 3.0 => Some(Point::new(fx, fy)),
        _ => Some(Point::new(p.cx, p.cy)),
    }
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
        assert!(found[0].score >= found[1].score);
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
}
