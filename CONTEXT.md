# Context: Image Tracker

Glossary of domain terms. No implementation details.

## Terms

### Bar Path
The trajectory of the barbell across a lift, as a sequence of positions over time. The primary output of the MVP.

### Seed
A user-placed point marking the object to track (e.g., the end of the barbell) on a chosen starting frame.

### Marker
A physical colored dot/tape placed on the barbell before filming. Optional: when present, the Color Tracker follows it; when absent, the Template Tracker follows the bar's own appearance (e.g., the plate end-face).

### Color Model
The color signature learned by sampling pixels around the Seed click. No fixed color names — whatever the user marked, the model represents it.

### Marker Color Advisor
An analysis of a video's overall palette that recommends which physical marker colors would contrast best in that scene, to guide future filming.

### Calibration
The mapping from image pixels to real-world meters, derived from one user-marked segment of known length (e.g., a standard 450mm plate diameter). Required for any metric in meters or m/s.

### Gap
A run of frames where the Marker could not be detected (occlusion, blur, out of frame). Short gaps are coasted over and interpolated in the Bar Path but flagged; metrics exclude or flag interpolated samples. A gap longer than the coast limit pauses tracking and asks the user to re-place the Seed.

### Lost
A terminal tracking state, distinct from a Gap's pause: the tracker keeps reporting a position (never misses) but its own identity confidence — the anchor score against the never-changing Seed patch — stays low for a sustained run of consecutive frames, meaning it has likely locked onto the wrong thing rather than merely losing sight of the object. Unlike a Gap's `NeedsReseed` pause, which recovers by re-placing the Seed at roughly the same spot, a headless run does not auto-resume out of Lost — doing so would just reseed from the same untrustworthy position. The run ends there with the honest partial Bar Path collected so far; a human can still manually reseed and continue.

### Preprocessor
A noise-reduction filter (e.g. Gaussian blur, median) applied to image regions before matching. Must be applied identically to the Seed's reference patch and to every candidate region — reference and candidates must live in the same filtered space for scores to be comparable.

### Tracking Strategy
The full recipe used to process one video: which Tracker, which Preprocessor chain, and the tuning thresholds. Strategies are swappable per video, and can be compared on a short segment to pick what fits a particular clip.

### Rep
One repetition of the lift, segmented from the Bar Path by vertical velocity sign: an eccentric phase (descent) followed by a concentric phase (ascent). Per-rep metrics include depth, peak concentric velocity, and mean concentric velocity.

### Overlay Video
The rendered output video: original frames plus the traced Bar Path, marker legend, and metrics.

### Tracker
Anything that, given a Seed and successive frames, produces the object's position in each frame. v1 ships two: the Color Tracker (follows a Marker via its Color Model) and the Template Tracker (follows appearance via correlation, for footage without a Marker). Joint (pose) tracking is a future Tracker, out of scope.
