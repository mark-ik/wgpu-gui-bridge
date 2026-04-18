# Changelog

All notable changes to this project will be documented here.

## [Unreleased]

### Added

- `README.md`: clarify that `servo-0.0.6-wgpu-28` is the Servo `v0.0.6` plus
  host `wgpu 28` compatibility branch and document the branch matrix

- `demo-servo-xilem`: Servo embedded in Xilem 0.4 with URL bar, CPU readback,
  and full input forwarding (mouse, scroll, keyboard)
- `demo-servo-iced`: Servo embedded in iced 0.14 with URL bar, CPU readback,
  flicker-free GPU upload via `image::allocate()`, and full input forwarding
- `demo-servo-gpui`: Servo embedded in GPUI 0.2 (Zed's framework) with URL bar,
  RGBA→BGRA conversion, `request_animation_frame()` render loop, and full input
  forwarding including custom key mapping
- `demo-servo-winit`: added mouse, scroll, and keyboard input forwarding to
  Servo; pages are now fully interactive (links, scrolling, text input)
- `rust-toolchain.toml`: pin workspace to Rust 1.92.0 (required by wgpu 28)
- `patches/gpui`: local gpui fork with taffy `=0.9.0` → `0.9.2` for
  compatibility with servo-layout
- `patches/serde_fmt`: local serde_fmt fork removing ambiguous `From` impl
  that breaks stylo's `ToCss` derive on Rust 1.92
- `wgpu-native-texture-interop`: public API doc comments on all major types
  (`InteropBackend`, `CapabilityMatrix`, `NativeFrame`, `ImportOptions`, etc.)
- `wgpu-native-texture-interop`: `#[non_exhaustive]` on `NativeFrame`,
  `NativeFrameKind`, `InteropBackend`, `SyncMechanism`, `InteropError`, and
  `UnsupportedReason` to protect downstream users from semver breaks
- `wgpu-native-texture-interop`, `servo-wgpu-interop-adapter`: crate-level
  `#![doc = include_str!("../README.md")]` so docs.rs renders the README

### Fixed

- `raw_gl/linux.rs`, `raw_gl/windows.rs`: Vulkan memory allocation now
  correctly queries `get_physical_device_memory_properties` and selects a
  `DEVICE_LOCAL` memory type index compatible with the image's
  `memory_type_bits`, rather than unconditionally using index 0

## [0.1.0] — Initial release

- GL→wgpu texture interop for Linux/Android (Vulkan opaque FD) and Apple
  (IOSurface→Metal)
- Windows Vulkan path (opaque Win32 NT handle) — builds and runs; depends on
  driver support for `VK_KHR_external_memory_win32` under WGL/EGL
- `wgpu-native-texture-interop`: core library with trait-based API
- `servo-wgpu-interop-adapter`: Servo `RenderingContext` integration
- `demo-raw-gl`: standalone glutin+glow FBO → wgpu demo (no Servo required)
- `demo-servo-winit`: full Servo + winit + wgpu reference application
