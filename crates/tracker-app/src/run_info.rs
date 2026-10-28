//! `<stem>.run.json`: provenance sidecar for auto (unseeded) `track` runs.
//!
//! The export `<stem>.json`/`.csv`/`.reps.json` shapes are deliberately left
//! alone; what the auto pre-pass decided (lift type, seed, plate size) and
//! that a human has not confirmed it lives here instead. Manual (seeded) runs
//! write no sidecar, and a missing sidecar means "verified" (older runs).
//! File IO lives in this crate; `tracker-core` stays pure.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracker_core::LiftType;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    /// `deadlift` | `squat` | `bench` | `unknown`.
    pub lift: String,
    pub lift_reason: String,
    pub verified: bool,
    pub seed_frame: u64,
    pub seed: [f64; 2],
    pub plate_px: f64,
}

/// Stable lowercase name used in the sidecar and the manifest.
pub fn lift_name(lift: LiftType) -> &'static str {
    match lift {
        LiftType::Squat => "squat",
        LiftType::Deadlift => "deadlift",
        LiftType::Bench => "bench",
        LiftType::Unknown => "unknown",
    }
}

/// `<stem>.<anything>` -> `<stem>.run.json` next to it.
pub fn sidecar_path(sibling_file: &Path) -> PathBuf {
    sibling_file.with_extension("run.json")
}

pub fn write_run_info(path: &Path, info: &RunInfo) -> Result<(), String> {
    let text = serde_json::to_string_pretty(info)
        .map_err(|e| format!("failed to serialize run info: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// `Ok(None)` when the sidecar does not exist (old or manual run => verified).
pub fn read_run_info(path: &Path) -> Result<Option<RunInfo>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} is not valid run info: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_info_roundtrips_through_the_sidecar() {
        let dir = std::env::temp_dir().join(format!("run-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = sidecar_path(&dir.join("clip.csv"));
        assert_eq!(path, dir.join("clip.run.json"));
        let info = RunInfo {
            lift: lift_name(LiftType::Squat).into(),
            lift_reason: "bar dips below start".into(),
            verified: false,
            seed_frame: 40,
            seed: [123.5, 456.0],
            plate_px: 88.25,
        };
        write_run_info(&path, &info).unwrap();
        assert_eq!(read_run_info(&path).unwrap(), Some(info));
    }

    #[test]
    fn absent_sidecar_reads_as_none_and_corrupt_is_an_error() {
        let dir = std::env::temp_dir().join(format!("run-info-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_run_info(&dir.join("none.run.json")).unwrap(), None);
        std::fs::write(dir.join("bad.run.json"), "{oops").unwrap();
        let e = read_run_info(&dir.join("bad.run.json")).unwrap_err();
        assert!(e.contains("bad.run.json"), "{e}");
    }
}
