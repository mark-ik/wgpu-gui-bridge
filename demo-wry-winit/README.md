# demo-wry-winit

Minimal host probe for `wry-wgpu-interop-adapter`.

This demo creates a real Wry webview and proves the host side of the decision tree first:

1. create a winit window,
2. initialize a host `wgpu` device,
3. wrap it in `HostWgpuContext`,
4. ask `wry-wgpu-interop-adapter` which web-surface mode is viable,
5. capture the host window as a stand-in `GraphicsCaptureItem`,
6. capture Wry's WebView2 child HWND through the controller exposed by `WebViewExtWindows`,
7. create a direct WebView2 `ICoreWebView2CompositionController` probe,
8. feed its Windows composition visual into `capture_visual_frame_once`,
9. import the captured shared texture into wgpu.

The Wry probe still exercises the native child-window WebView2 path. The direct composition-controller probe is separate because Wry exposes the normal `ICoreWebView2Controller`, not `ICoreWebView2CompositionController`.

Current Windows runtime observations:

- top-level HWND capture imports successfully,
- Wry's WebView2 child HWND is rejected by `IGraphicsCaptureItemInterop::CreateForWindow` with `0x80070057`,
- the direct WebView2 `ICoreWebView2CompositionController` probe completes navigation and a renderer animation-frame wait,
- WebView2 `CapturePreview` produces a valid PNG snapshot from the composition-controller page,
- a plain WinComp sprite visual in the same desktop target also produces a valid `GraphicsCaptureItem` size of `420x260`,
- both the WebView target visual and its root visual produce a valid `GraphicsCaptureItem` size of `420x260`,
- after laying out the Wry child HWND and direct composition target side by side, a WebView content mutation after `GraphicsCaptureSession::StartCapture` yields a captured `420x260` `Bgra8Unorm` WebView target visual frame,
- `TryGetNextFrame` currently still times out for the plain sprite visual without producing a frame.

On Windows, the probe requests the DX12 backend because the intended WebView2 capture path feeds `NativeFrame::Dx12SharedTexture`.

Run:

```bash
cargo run -p demo-wry-winit
```
