# servo-wgpu-interop-adapter

Servo-specific offscreen rendering adapter built on [`wgpu-native-texture-interop`](../wgpu-native-texture-interop/).

This crate bridges Servo's rendering context to the host application. It provides three things:

1. **`ServoWgpuRenderingContext`** — an offscreen `RenderingContext` that Servo renders into. Supports CPU readback via `read_full_frame()` (returns an `image::RgbaImage` of the current page).
2. **`ServoWgpuInteropAdapter`** — zero-copy GPU import path that imports Servo's GL framebuffer directly into a host `wgpu::Texture` via the core interop crate.
3. **`SurfmanSurfaceImporter`** — a surfman-surface import transaction helper for integrations like Servo paint that already own swap-chain surfaces and just need the bind/current/frame/import/unbind sequence packaged behind one bridge API.

## Which path to use

- **CPU readback** (`ServoWgpuRenderingContext::read_full_frame()`): Works on all platforms. Simpler to integrate — just display the returned image in your framework's image widget. Adds a GPU→CPU→GPU round-trip per frame.
- **GPU import** (`ServoWgpuInteropAdapter`): Zero-copy, but requires compatible GL drivers and a Vulkan/Metal wgpu backend. Currently blocked on Windows because Servo forces ANGLE (D3D-backed GL), whose textures can't be shared with wgpu.

The CPU readback demos ([xilem](../demo-servo-xilem/), [iced](../demo-servo-iced/), [gpui](../demo-servo-gpui/)) use `read_full_frame()`. The [winit demo](../demo-servo-winit/) tries GPU import first and falls back to CPU readback.

## Feature flags

- **`servo`** (optional) — enables the published `servo` crate dependency and Servo trait implementations. All Servo-embedding demos enable this feature.

Without `servo`, the surfman-level types are still available, including `SurfmanSurfaceImporter`, which makes the crate usable from Servo-adjacent integrations without pulling in the published `servo` crate.

## Usage

```toml
[dependencies]
servo-wgpu-interop-adapter = { version = "0.1", features = ["servo"] }
servo = "0.1.0"
```

```rust
use servo_wgpu_interop_adapter::ServoWgpuRenderingContext;

// Create the rendering context (implements Servo's RenderingContext trait)
let render_ctx = ServoWgpuRenderingContext::new(connection, adapter, surface_type);

// After Servo paints, read the frame as an RGBA image
if let Some(rgba_image) = render_ctx.read_full_frame() {
    // Display in your framework's image widget
}
```

See the demo crates for complete integration examples.

## License

MIT OR Apache-2.0
