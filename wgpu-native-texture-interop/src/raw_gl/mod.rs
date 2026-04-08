//! Raw GL-to-wgpu import primitives with no surfman dependency.
//!
//! These functions accept raw GL handles (FBO IDs, IOSurface pointers, etc.)
//! and produce `wgpu::Texture` objects. They can be used by any GL producer,
//! not just surfman-based ones.

#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod linux;

#[cfg(target_vendor = "apple")]
pub mod metal;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub mod dx12;

#[cfg(target_os = "windows")]
pub mod angle_d3d11;

/// BGRA-to-RGBA and Y-flip normalization pass for imported textures.
pub mod texture_normalizer;

pub mod producer;
