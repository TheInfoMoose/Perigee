//! `perigee-client`: Remote Viewer Session Pipeline Crate.
//!
//! Status: stand-in. Video decoding, `wgpu` rendering, and audio device playback are stand-ins (wire payloads are forwarded undecoded, `render` only computes texture layouts, and `audio` only buffers frames in memory); Phase 5 replaces them.
//!
//! Coordinates frontend-agnostic connection establishment (`connect`), QUIC stream demuxing
//! (`bridge`, `session`), texture plane layout validation (`render`), in-memory audio frame
//! buffering (`audio`), input event forwarding, and sequence-loss feedback (`LossReport`, `FrameAck`).

pub mod audio;
pub mod bridge;
pub mod connect;
pub mod decode;
pub mod render;
pub mod session;
pub mod window;

pub use audio::*;
pub use bridge::*;
pub use connect::*;
pub use decode::*;
pub use render::*;
pub use session::*;
pub use window::{
    MIN_VIEWER_HEIGHT, MIN_VIEWER_WIDTH, ViewerWindowConfig, ViewportRect,
    compute_letterbox_viewport, format_window_title, run_viewer_window,
};
