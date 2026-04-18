# wgpu-gui-bridge

Rust workspace for embedding [Servo](https://servo.org) web content into host applications. It provides the low-level texture interop plumbing (GL/Vulkan/Metal/DX12 → wgpu) and a set of reference demos showing how to embed Servo in different GUI frameworks.

If you're looking to embed a web renderer in your Rust application, start with the demo closest to your stack and adapt from there. No promises! These are generated reference implementations to see what's possible, but the core interop crate is hopefully reusable and framework-agnostic.

Also, to be clear and upfront, I used AI for pretty much all of it, adapting the Slint folks' Servo embedding example, and I think it turned out pretty well, considering. The demos are a bit rough but should be straightforward to understand and adapt. I wanted to see Servo in some more esoteric GUI frameworks, but I don't have Linux or Mac hardware to test those, so contributions are very welcome!

NOTE: Though I have completed my goal for my webrender-wgpu fork and am now using and refining my lil wgpu backend in my servo-wgpu fork, please understand that this bridge will likely change to accomodate my principal use for it: the main blocker for servo-wgpu was that I'd have to do cpu readback for webgl content. But with this nifty bridge, servo can output entirely wgpu, with just the webgl content going through this bridge and rendering composited into wgpu texture. I believe this to be a pretty good idea because I am not porting webgl to wgpu, but annoyingly we keep mozangle and surfman for the GL context. 

## Crates

| Crate | Purpose |
| --- | --- |
| [`wgpu-native-texture-interop`](wgpu-native-texture-interop/) | Core library: imports native GPU textures (GL FBO, Vulkan image, Metal IOSurface) into host-owned `wgpu` textures. Framework-agnostic, no Servo dependency required. |
| [`servo-wgpu-interop-adapter`](servo-wgpu-interop-adapter/) | Servo-specific adapter: wraps Servo's offscreen rendering context and bridges it to the core interop crate. Provides `ServoWgpuRenderingContext` for CPU readback and `ServoWgpuInteropAdapter` for zero-copy GPU import. |

## Demos

Each demo embeds Servo in a different Rust GUI framework to show that the approach generalizes. All demos include a URL bar, clickable links, scrolling, and keyboard input forwarding.

| Demo | Framework | Rendering path | Notes |
| --- | --- | --- | --- |
| [`demo-servo-winit`](demo-servo-winit/) | winit + wgpu (no toolkit) | GPU import with CPU fallback | Bare-minimum embedding. No URL bar UI — pass URLs via CLI. Primary reference for the interop layer. |
| [`demo-servo-xilem`](demo-servo-xilem/) | [Xilem](https://github.com/linebender/xilem) 0.4 | CPU readback | Reactive UI with URL bar. Uses masonry/peniko for image display. |
| [`demo-servo-iced`](demo-servo-iced/) | [iced](https://github.com/iced-rs/iced) 0.14 | CPU readback | Elm-architecture UI with URL bar. Uses `image::allocate()` to avoid async upload flicker. |
| [`demo-servo-gpui`](demo-servo-gpui/) | [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 0.2 | CPU readback | Zed's UI framework. RGBA→BGRA conversion for GPUI's `RenderImage`. |
| [`demo-raw-gl`](demo-raw-gl/) | glutin + glow | GPU import | Standalone GL→wgpu demo (spinning triangle). No Servo dependency — proves the interop layer works independently. |

### Rendering paths

**GPU import (zero-copy):** Servo renders to a GL framebuffer, which is imported directly into a host `wgpu` texture via platform-specific interop (Vulkan external memory, Metal IOSurface). Fastest path, but requires compatible GL drivers.

**CPU readback (fallback):** Servo renders offscreen, pixels are read back to CPU via `read_full_frame()`, then uploaded to the host's image widget. Works everywhere but adds a GPU→CPU→GPU round-trip per frame. This is the path used by the xilem, iced, and GPUI demos today. On Windows, this is currently the only path because Servo forces ANGLE, whose D3D textures can't be shared with wgpu's Vulkan/DX12 textures.

## Quick start

```bash
# Core crate tests
cargo test -p wgpu-native-texture-interop

# Build check (requires Servo git dependency)
cargo check -p servo-wgpu-interop-adapter --features servo

# Run a demo
cargo run -p demo-servo-winit
cargo run -p demo-servo-xilem
cargo run -p demo-servo-iced
cargo run -p demo-servo-gpui
cargo run -p demo-raw-gl
```

Pass a URL to any Servo demo:

```bash
cargo run -p demo-servo-winit -- https://servo.org
cargo run -p demo-servo-iced -- https://example.com
```

## Branches

The repository is organized around Servo compatibility lines so embedders can
pick a branch without digging through commit history.

| Branch | Purpose | Servo line |
| --- | --- | --- |
| `main` | Recommended default for embedders | current Servo LTS release line |
| `latest-release` | Tracks the newest non-LTS Servo release once one exists beyond the current LTS line | newest post-LTS release line |
| `experimental` | Integration work against upstream Servo head | upstream `main` |
| `servo-wgpu` | Fork-specific work for the WebRender wgpu backend and related experiments | custom forks |

`main` is the branch most users should follow. `latest-release` only diverges
once Servo ships a newer stable, non-LTS release beyond the current LTS line.

## Platform support

| Platform | GPU import | CPU readback | Notes |
| --- | --- | --- | --- |
| Linux | GL FBO → Vulkan image → wgpu | Yes | Primary development target |
| macOS | IOSurface → Metal → wgpu | Yes | |
| Windows | Builds, blocked at runtime | Yes | Servo forces ANGLE (D3D); ANGLE textures can't be shared with wgpu's Vulkan/DX12. CPU readback works. |

## Prerequisites

- **Rust 1.92+** (pinned in `rust-toolchain.toml`; required by wgpu 29)
- **Servo current LTS release** on `main` (resolved via Cargo dependency)
- **Windows**: ANGLE DLLs (`libEGL.dll`, `libGLESv2.dll`) must be next to the executable at runtime. They're built by `mozangle` during compilation — find them in `target/debug/build/mozangle-*/out/` and copy to `target/debug/`. If using a custom `CARGO_TARGET_DIR`, copy them there too.
- **Windows without nasm**: set `AWS_LC_SYS_NO_ASM=1` before building (Servo pulls `aws-lc-rs`).

## How to embed Servo in your own application

The demos are designed as copy-and-adapt references. The general pattern:

1. **Add dependencies**: `servo`, `servo-wgpu-interop-adapter` (with `features = ["servo"]`), and your GUI framework.
2. **Initialize Servo**: Create a `ServoWgpuRenderingContext`, build a `Servo` instance with `ServoBuilder`, create a `WebView` with `WebViewBuilder`, and navigate to a URL.
3. **Pump the event loop**: Call `servo.spin_event_loop()` each frame to let Servo process network/layout/paint work.
4. **Read frames**: Call `render_context.read_full_frame()` to get an `RgbaImage` of the current page.
5. **Display**: Convert the image to your framework's image type and display it.
6. **Forward input**: Convert your framework's mouse/keyboard events to Servo's `InputEvent` types and call `webview.notify_input_event()`.

See [`demo-servo-iced/src/main.rs`](demo-servo-iced/src/main.rs) for a clean example of this pattern, or [`demo-servo-winit/src/main.rs`](demo-servo-winit/src/main.rs) for the GPU import path.

## Workspace patches

The `patches/` directory contains local forks of two crates needed to resolve dependency conflicts:

- **`patches/gpui`**: Changes gpui's `taffy` dependency from `=0.9.0` to `0.9.2` so it coexists with servo-layout's `taffy ^0.9.2`.
- **`patches/serde_fmt`**: Removes an `impl From<serde_fmt::Error> for std::fmt::Error` that creates ambiguous type resolution in stylo's `ToCss` derive macro on Rust 1.92.

These patches are only needed by `demo-servo-gpui`. The other demos and the core crates don't require them.

## License

MIT OR Apache-2.0
