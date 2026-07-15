//! Geometry primitives: `Point` and `Frame` (owned RGB pixel buffer).

use std::fmt;

/// A 2D point in image-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Errors that can occur when constructing a `Frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The provided byte buffer length does not match `width * height * 3`.
    BufferLengthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BufferLengthMismatch { expected, actual } => write!(
                f,
                "frame buffer length mismatch: expected {expected} bytes, got {actual}"
            ),
        }
    }
}

/// An owned RGB image buffer (8-bit per channel, no padding).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Frame {
    /// Construct a `Frame` from raw RGB bytes.
    ///
    /// `rgb` must have exactly `width * height * 3` bytes, otherwise
    /// `FrameError::BufferLengthMismatch` is returned.
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Result<Self, FrameError> {
        let expected = width as usize * height as usize * 3;
        if rgb.len() != expected {
            return Err(FrameError::BufferLengthMismatch {
                expected,
                actual: rgb.len(),
            });
        }
        Ok(Self { width, height, rgb })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the RGB triple at `(x, y)`, or `None` if out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 3;
        Some([self.rgb[idx], self.rgb[idx + 1], self.rgb[idx + 2]])
    }

    /// The raw interleaved RGB buffer (`width * height * 3` bytes, no
    /// padding). For adapters that need to hand pixels to something else
    /// wholesale (e.g. an egui texture upload) rather than pixel-by-pixel.
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_construction_and_equality() {
        let a = Point::new(1.5, -2.0);
        let b = a; // Copy
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), "Point { x: 1.5, y: -2.0 }");
    }

    #[test]
    fn frame_new_rejects_wrong_buffer_length() {
        let result = Frame::new(2, 2, vec![0u8; 5]);
        assert_eq!(
            result,
            Err(FrameError::BufferLengthMismatch {
                expected: 12,
                actual: 5
            })
        );
    }

    #[test]
    fn frame_new_accepts_correct_buffer_length() {
        let frame = Frame::new(2, 1, vec![10, 20, 30, 40, 50, 60]).unwrap();
        assert_eq!(frame.width(), 2);
        assert_eq!(frame.height(), 1);
    }

    #[test]
    fn frame_pixel_reads_correct_rgb_triple() {
        let frame = Frame::new(2, 1, vec![10, 20, 30, 40, 50, 60]).unwrap();
        assert_eq!(frame.pixel(0, 0), Some([10, 20, 30]));
        assert_eq!(frame.pixel(1, 0), Some([40, 50, 60]));
    }

    #[test]
    fn frame_pixel_out_of_bounds_returns_none() {
        let frame = Frame::new(2, 1, vec![0; 6]).unwrap();
        assert_eq!(frame.pixel(2, 0), None);
        assert_eq!(frame.pixel(0, 1), None);
    }

    #[test]
    fn frame_error_display_message() {
        let err = FrameError::BufferLengthMismatch {
            expected: 12,
            actual: 5,
        };
        assert_eq!(
            err.to_string(),
            "frame buffer length mismatch: expected 12 bytes, got 5"
        );
    }
}
