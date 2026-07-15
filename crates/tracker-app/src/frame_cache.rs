//! Seek-based frame cache for scrubbing (task 2.3).
//!
//! The 2.2 `FrameSource` is forward-only streaming. For a scrub bar we need
//! random access, but a lift video can be ~2000 frames at 1024x576x3 bytes
//! (~1.7MB/frame) — caching every decoded frame at full resolution would be
//! multiple GB, which is not acceptable.
//!
//! Chosen strategy: **seek + small LRU**. Each frame is decoded on demand by
//! re-spawning `ffmpeg` with `-ss <seek-seconds> -vframes 1` (see
//! `SeekingFrameDecoder` in `seek_source.rs`), and the last `capacity`
//! decoded frames are kept in an LRU cache so re-visiting nearby frames
//! (dragging the scrub slider back and forth) doesn't re-decode every time.
//! With `capacity = 16` and a 1024x576 frame that's ~27MB, independent of
//! total video length.
//!
//! This module holds the *pure* logic (index clamping, cache eviction
//! policy) behind a `FrameDecoder` trait so it is unit-testable without a
//! real ffmpeg process or a GUI.

/// Anything that can decode a single frame by index. Implemented by
/// `SeekingFrameDecoder` (real ffmpeg) in `seek_source.rs`; tests use a
/// fake that counts calls.
pub trait FrameDecoder {
    type Error;
    type Frame;

    /// Decode and return the frame at `index` (0-based).
    fn decode_frame(&mut self, index: u64) -> Result<Self::Frame, Self::Error>;
}

/// Clamp a requested frame index into `[0, frame_count - 1]`.
/// `frame_count == 0` clamps everything to `0` (caller must not treat that
/// as a valid frame in an empty video; checked separately by callers that
/// know the video's actual length).
pub fn clamp_frame_index(requested: i64, frame_count: u64) -> u64 {
    if frame_count == 0 {
        return 0;
    }
    let max = frame_count - 1;
    if requested < 0 {
        0
    } else if requested as u64 > max {
        max
    } else {
        requested as u64
    }
}

/// A small least-recently-used cache of decoded frames, keyed by frame
/// index. Wraps a `FrameDecoder` and only calls it on a cache miss.
pub struct FrameCache<D: FrameDecoder> {
    decoder: D,
    capacity: usize,
    /// Most-recently-used at the back.
    order: Vec<u64>,
    entries: std::collections::HashMap<u64, D::Frame>,
}

impl<D: FrameDecoder> FrameCache<D>
where
    D::Frame: Clone,
{
    pub fn new(decoder: D, capacity: usize) -> Self {
        assert!(capacity > 0, "cache capacity must be at least 1");
        Self {
            decoder,
            capacity,
            order: Vec::new(),
            entries: std::collections::HashMap::new(),
        }
    }

    /// Number of frames currently held in the cache (test/introspection hook).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the frame at `index`, decoding and caching it on a miss, evicting
    /// the least-recently-used entry if the cache is full.
    pub fn get(&mut self, index: u64) -> Result<D::Frame, D::Error> {
        if let Some(frame) = self.entries.get(&index) {
            let frame = frame.clone();
            self.touch(index);
            return Ok(frame);
        }

        let frame = self.decoder.decode_frame(index)?;

        if self.entries.len() >= self.capacity && !self.entries.contains_key(&index) {
            if let Some(lru) = self.order.first().copied() {
                self.order.remove(0);
                self.entries.remove(&lru);
            }
        }
        self.entries.insert(index, frame.clone());
        self.order.push(index);

        Ok(frame)
    }

    fn touch(&mut self, index: u64) {
        if let Some(pos) = self.order.iter().position(|&i| i == index) {
            self.order.remove(pos);
        }
        self.order.push(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_within_range_is_unchanged() {
        assert_eq!(clamp_frame_index(5, 10), 5);
    }

    #[test]
    fn clamp_negative_becomes_zero() {
        assert_eq!(clamp_frame_index(-3, 10), 0);
    }

    #[test]
    fn clamp_past_end_becomes_last_index() {
        assert_eq!(clamp_frame_index(100, 10), 9);
    }

    #[test]
    fn clamp_empty_video_is_zero() {
        assert_eq!(clamp_frame_index(0, 0), 0);
        assert_eq!(clamp_frame_index(50, 0), 0);
    }

    #[test]
    fn clamp_last_valid_index_is_unchanged() {
        assert_eq!(clamp_frame_index(9, 10), 9);
    }

    /// Fake decoder that records how many times each index was decoded, so
    /// tests can assert on cache hits vs. misses without touching ffmpeg.
    struct CountingDecoder {
        calls: std::collections::HashMap<u64, u32>,
    }

    impl CountingDecoder {
        fn new() -> Self {
            Self {
                calls: std::collections::HashMap::new(),
            }
        }

        fn call_count(&self, index: u64) -> u32 {
            *self.calls.get(&index).unwrap_or(&0)
        }
    }

    impl FrameDecoder for CountingDecoder {
        type Error = ();
        type Frame = u64; // stand-in "frame": just the index, doubled below

        fn decode_frame(&mut self, index: u64) -> Result<Self::Frame, Self::Error> {
            *self.calls.entry(index).or_insert(0) += 1;
            Ok(index * 10)
        }
    }

    #[test]
    fn miss_decodes_and_caches() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 4);
        assert_eq!(cache.get(3).unwrap(), 30);
        assert_eq!(cache.decoder.call_count(3), 1);
    }

    #[test]
    fn repeated_get_is_a_cache_hit_not_a_redecode() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 4);
        cache.get(3).unwrap();
        cache.get(3).unwrap();
        cache.get(3).unwrap();
        assert_eq!(cache.decoder.call_count(3), 1);
    }

    #[test]
    fn evicts_least_recently_used_when_full() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 2);
        cache.get(1).unwrap();
        cache.get(2).unwrap();
        cache.get(3).unwrap(); // evicts 1 (least recently used)

        assert_eq!(cache.len(), 2);
        cache.get(1).unwrap(); // must re-decode: it was evicted
        assert_eq!(cache.decoder.call_count(1), 2);
        // 2 and 3 should still be cached (not re-decoded before this point).
        assert_eq!(cache.decoder.call_count(2), 1);
        assert_eq!(cache.decoder.call_count(3), 1);
    }

    #[test]
    fn get_refreshes_recency_so_it_survives_eviction() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 2);
        cache.get(1).unwrap();
        cache.get(2).unwrap();
        cache.get(1).unwrap(); // touch 1: now 2 is least-recently-used
        cache.get(3).unwrap(); // should evict 2, not 1

        cache.get(1).unwrap();
        assert_eq!(cache.decoder.call_count(1), 1, "1 should still be cached");
        cache.get(2).unwrap();
        assert_eq!(cache.decoder.call_count(2), 2, "2 should have been evicted");
    }

    #[test]
    fn cache_never_exceeds_capacity() {
        let mut cache = FrameCache::new(CountingDecoder::new(), 3);
        for i in 0..20 {
            cache.get(i).unwrap();
            assert!(cache.len() <= 3);
        }
    }
}
