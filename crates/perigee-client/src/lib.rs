//! `perigee-client`: Remote Viewer Session Pipeline Crate.
//!
//! Status: stand-in. Video decoding, `wgpu` rendering, and audio device playback are stand-ins (wire payloads are forwarded undecoded, `render` only computes texture layouts, and `audio` only buffers frames in memory); Phase 5 replaces them.
//!
//! Coordinates QUIC stream demuxing (`bridge`, `session`), texture plane layout validation
//! (`render`), in-memory audio frame buffering (`audio`), input event forwarding, and
//! sequence-loss feedback (`LossReport`, `FrameAck`).

pub mod audio;
pub mod bridge;
pub mod render;
pub mod session;

pub use audio::*;
pub use bridge::*;
pub use render::*;
pub use session::*;
