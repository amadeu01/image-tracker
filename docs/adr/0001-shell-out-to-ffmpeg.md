# 0001 — Shell out to ffmpeg binary for video decode/encode

## Status
Accepted (2026-07-14)

## Context
The tool must decode MP4 (H.264) input and encode an overlay MP4 output. Options:
1. Link system ffmpeg libraries via `video-rs`/`ffmpeg-next` — fastest, but requires dev headers, causes build/version pain across macOS and Linux.
2. Spawn the `ffmpeg` CLI and stream raw RGB frames over pipes — only requires an ffmpeg binary on PATH.
3. Pure-Rust decoding — no production-ready H.264/MP4 story exists.

Target platforms are macOS and Linux, where installing the ffmpeg binary is trivial.

## Decision
Spawn the `ffmpeg` binary as a subprocess, decoding to raw frames over stdout and encoding the overlay video over stdin. Video IO is hidden behind traits so a linked-library backend can replace it later without touching the domain.

## Consequences
- No C toolchain or ffmpeg headers needed to build; `cargo build` just works.
- Runtime requirement: `ffmpeg` on PATH (checked at startup with a clear error).
- Frame IO is marginally slower than in-process decoding; irrelevant for short lift videos.
- Encoding the output overlay video uses the same mechanism for free.
