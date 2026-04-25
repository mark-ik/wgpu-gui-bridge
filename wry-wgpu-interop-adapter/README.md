# wry-wgpu-interop-adapter

Capability-driven system-webview adapter for `wgpu-gui-bridge`.

This crate is the intended home for Wry/WebView-backed frame production. It is deliberately separate from `wgpu-native-texture-interop`: the native interop crate imports GPU resources, while this adapter owns system-webview probing, fallback selection, and eventual Wry/WebView2/WKWebView/WebKitGTK frame-source integration.

## Current slice

The crate currently provides the shared contract:

- `WebSurfaceMode` — imported texture, native child overlay, CPU snapshot, or unsupported.
- `WryWebSurfaceCapabilities` — platform/backend capability reporting.
- `WryWebSurfaceFrame` — imported native frame, CPU RGBA frame, PNG snapshot, or overlay-only state.
- `WryWebSurfaceProducer` — producer trait that future WebView2/WK/WebKitGTK implementations will satisfy.
- Windows planning helpers that describe the WebView2 CompositionController plus `Windows.Graphics.Capture` path and identify the D3D11-to-D3D12 bridge as the first hard proof point.

## Windows target path

The intended Windows producer is:

```text
WebView2 CompositionController visual
  -> Windows.Graphics.Capture frame pool
  -> ID3D11Texture2D
  -> shared D3D/DXGI handle or D3D11On12 copy
  -> NativeFrame::Dx12SharedTexture
  -> WgpuTextureImporter
```

WebView2 `TextureStream` is not treated as the primary path because it is a page/media texture stream API, not a whole-webview compositor-output API.

The Windows module now contains the first concrete bridge helper:

- `D3D11SharedTextureFactory::create_shared_texture_frame(...)` allocates an NT-handle-shareable D3D11 texture.
- `export_capture_frame_shared_handle(...)` attempts to export an existing captured `ID3D11Texture2D`.
- `D3D11SharedTextureFactory::copy_capture_into_shared_frame(...)` copies a captured D3D11 texture into an adapter-owned shared texture when direct export is not available.
- `capture_graphics_item_frame_once(...)` captures any `GraphicsCaptureItem` into the shared-texture bridge.
- `capture_visual_frame_once(...)` accepts a raw `Windows.UI.Composition.Visual` pointer and routes it through `GraphicsCaptureItem::CreateFromVisual`.

The demo currently validates the downstream path with a host-window capture item. The next proof point is to obtain a real WebView2 `CompositionController` visual, either through Wry if it exposes the necessary hook or through a direct WebView2 host shim.

## Fallbacks

`NativeChildOverlay` remains the normal Wry fallback. `CpuSnapshot` is useful for diagnostics, thumbnails, and low-frequency preview paths, but it is not the target for interactive composited web surfaces.
