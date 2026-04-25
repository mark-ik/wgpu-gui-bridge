# demo-wry-winit

Minimal host probe for `wry-wgpu-interop-adapter`.

This demo does not create a Wry webview yet. Its job is to prove the host side of the decision tree first:

1. create a winit window,
2. initialize a host `wgpu` device,
3. wrap it in `HostWgpuContext`,
4. ask `wry-wgpu-interop-adapter` which web-surface mode is viable,
5. capture the host window as a stand-in `GraphicsCaptureItem`,
6. import the captured shared texture into wgpu.

On Windows, the probe requests the DX12 backend because the intended WebView2 capture path feeds `NativeFrame::Dx12SharedTexture`.

Run:

```bash
cargo run -p demo-wry-winit
```
