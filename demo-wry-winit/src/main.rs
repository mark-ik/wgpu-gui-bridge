//! Minimal winit + wgpu host probe for Wry/system-webview texture interop.

use std::sync::Arc;

#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wgpu_native_texture_interop::{
    HostWgpuContext, ImportOptions, TextureImporter, WgpuTextureImporter,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;
use wry::{Rect, WebViewBuilder};
use wry_wgpu_interop_adapter::{
    OverlayOnlyProducer, WebSurfaceMode, WryWebSurfaceFrame, WryWebSurfaceProducer,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        probe_only: std::env::args().any(|arg| arg == "--probe-only"),
        state: None,
    };
    Ok(event_loop.run_app(&mut app)?)
}

#[derive(Default)]
struct App {
    probe_only: bool,
    state: Option<AppState>,
}

struct AppState {
    window: Arc<Window>,
    _webview: wry::WebView,
    _device: wgpu::Device,
    _queue: wgpu::Queue,
    #[cfg(target_os = "windows")]
    renderer: Option<WebViewRenderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        match AppState::new(event_loop) {
            Ok(state) => {
                self.state = Some(state);
                if self.probe_only {
                    event_loop.exit();
                }
            }
            Err(error) => {
                eprintln!("demo-wry-winit: initialization failed: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            #[cfg(target_os = "windows")]
            WindowEvent::Resized(new_size) => {
                if let Some(state) = self.state.as_mut() {
                    if let Some(renderer) = state.renderer.as_mut() {
                        renderer.resize(new_size);
                    }
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::RedrawRequested => {
                if let Some(state) = self.state.as_mut() {
                    if let Some(renderer) = state.renderer.as_mut() {
                        if let Err(error) = renderer.render() {
                            eprintln!("demo-wry-winit: render failed: {error}");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

impl AppState {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("demo-wry-winit")
                    .with_inner_size(winit::dpi::PhysicalSize::new(900, 600)),
            )?,
        );

        let (instance, device, queue, adapter_info) = pollster::block_on(create_host_device())?;
        let host = HostWgpuContext::new(device.clone(), queue.clone());
        let capabilities = wry_wgpu_interop_adapter::WryWebSurfaceCapabilities::probe(Some(&host));

        println!("wgpu adapter: {}", adapter_info.name);
        println!("wgpu backend: {:?}", host.backend);
        println!("system webview backend: {:?}", capabilities.backend);
        println!("preferred surface mode: {:?}", capabilities.preferred_mode);
        println!(
            "imported texture support: {:?}",
            capabilities.imported_texture
        );
        println!(
            "native overlay support: {:?}",
            capabilities.native_child_overlay
        );
        println!("CPU snapshot support: {:?}", capabilities.cpu_snapshot);
        println!("reason: {}", capabilities.reason);

        let mut producer = OverlayOnlyProducer::new(capabilities);
        let frame = producer.acquire_frame()?;
        println!("initial producer frame: {}", frame_label(&frame));

        let webview = WebViewBuilder::new()
            .with_html(WEBVIEW_PROBE_HTML)
            .with_bounds(Rect {
                position: wry::dpi::LogicalPosition::new(WRY_PROBE_X, PROBE_Y).into(),
                size: wry::dpi::LogicalSize::new(WRY_PROBE_WIDTH, WRY_PROBE_HEIGHT).into(),
            })
            .build_as_child(&window)?;

        #[cfg(target_os = "windows")]
        run_windows_shared_texture_probe(&window, &webview, &host)?;

        #[cfg(target_os = "windows")]
        let captured = run_webview2_composition_visual_probe(&window, &host)?;

        #[cfg(target_os = "windows")]
        let renderer = match captured {
            Some(captured) => Some(WebViewRenderer::new(
                instance,
                window.clone(),
                host.clone(),
                captured,
            )?),
            None => {
                drop(instance);
                None
            }
        };

        Ok(Self {
            window,
            _webview: webview,
            _device: device,
            _queue: queue,
            #[cfg(target_os = "windows")]
            renderer,
        })
    }
}

#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_X: f32 = 450.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_Y: f32 = 48.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_WIDTH: f32 = 420.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_HEIGHT: f32 = 260.0;

const PROBE_Y: i32 = 48;
const WRY_PROBE_X: i32 = 24;
const WRY_PROBE_WIDTH: u32 = 360;
const WRY_PROBE_HEIGHT: u32 = 360;

const WEBVIEW_PROBE_HTML: &str = r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        html, body {
            margin: 0;
            width: 100%;
            height: 100%;
            overflow: hidden;
            background: #101820;
            color: #f3efe0;
            font-family: system-ui, sans-serif;
        }
        body {
            display: grid;
            place-items: center;
        }
        main {
            display: grid;
            gap: 12px;
            text-align: center;
        }
        h1 {
            margin: 0;
            font-size: 28px;
            font-weight: 650;
            letter-spacing: 0;
        }
        p {
            margin: 0;
            color: #8fd2c7;
            font-size: 15px;
        }
    </style>
</head>
<body>
    <main>
        <h1>Wry WebView2 Probe</h1>
        <p>Rendered by a real system webview child surface.</p>
    </main>
</body>
</html>"#;

#[cfg(target_os = "windows")]
const COMPOSITION_WEBVIEW_PROBE_HTML: &str = r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        html, body {
            margin: 0;
            width: 100%;
            height: 100%;
            overflow: hidden;
            background: #17202a;
            color: #f8f1d8;
            font-family: system-ui, sans-serif;
        }
        body {
            display: grid;
            place-items: center;
            position: relative;
        }
        main {
            display: grid;
            gap: 10px;
            text-align: center;
            z-index: 1;
        }
        h1 {
            margin: 0;
            font-size: 26px;
            font-weight: 650;
            letter-spacing: 0;
        }
        p {
            margin: 0;
            color: #ffbe70;
            font-size: 14px;
        }
        #tick {
            position: absolute;
            top: 8px;
            left: 8px;
            font-size: 13px;
            color: #8fd2c7;
            font-variant-numeric: tabular-nums;
        }
        @keyframes sweep {
            0%   { transform: translateX(0); background: #ff6b6b; }
            50%  { background: #56cfe1; }
            100% { transform: translateX(calc(100% - 24px)); background: #ff6b6b; }
        }
        #bar {
            position: absolute;
            bottom: 12px;
            left: 12px;
            right: 12px;
            height: 6px;
            border-radius: 3px;
            background: #2c3e50;
            overflow: hidden;
        }
        #bar::after {
            content: "";
            display: block;
            width: 24px;
            height: 100%;
            background: #ff6b6b;
            animation: sweep 2.4s linear infinite;
        }
    </style>
</head>
<body>
    <div id="tick">frame 0</div>
    <main>
        <h1>CompositionController Probe</h1>
        <p>Rendered through a WebView2 composition visual.</p>
    </main>
    <div id="bar"></div>
    <script>
        let n = 0;
        const tick = document.getElementById("tick");
        function loop() {
            n++;
            tick.textContent = "frame " + n;
            requestAnimationFrame(loop);
        }
        requestAnimationFrame(loop);
    </script>
</body>
</html>"#;

#[cfg(target_os = "windows")]
fn run_windows_shared_texture_probe(
    window: &Window,
    webview: &wry::WebView,
    host: &HostWgpuContext,
) -> Result<(), Box<dyn std::error::Error>> {
    use wry::WebViewExtWindows;
    use wry_wgpu_interop_adapter::windows_capture::{
        D3D11SharedTextureFactory, DxgiSharedHandleBridge, capture_window_frame_once,
        close_shared_handle, probe_graphics_capture_prerequisites,
    };

    let graphics_capture = probe_graphics_capture_prerequisites()?;
    println!(
        "GraphicsCapture probe: session_supported={} winrt_d3d_device={} free_threaded_frame_pool={}",
        graphics_capture.session_supported,
        graphics_capture.winrt_d3d_device_created,
        graphics_capture.free_threaded_frame_pool_created
    );

    let factory = D3D11SharedTextureFactory::new_hardware()?;
    let shared = factory.create_shared_texture_frame(
        winit::dpi::PhysicalSize::new(64, 64),
        wgpu::TextureFormat::Bgra8Unorm,
        1,
    )?;
    let handle = shared.shared_handle;
    let dx12_frame = DxgiSharedHandleBridge.bridge_shared_handle(shared)?;
    println!("D3D11 shared texture probe: exported NT handle {handle:p}");

    let surface_frame = dx12_frame.into_surface_frame();
    let WryWebSurfaceFrame::Native(native_frame) = surface_frame else {
        return Err("D3D11 shared texture bridge did not produce a native frame".into());
    };
    let importer = WgpuTextureImporter::new(host.clone());
    let imported = importer.import_frame(&native_frame, &ImportOptions::default())?;
    println!(
        "D3D11 shared texture probe: imported {:?} {}x{} generation {}",
        imported.format, imported.size.width, imported.size.height, imported.generation
    );

    unsafe {
        close_shared_handle(handle)?;
    }

    let hwnd = hwnd_from_window(window)?;
    let captured = unsafe { capture_window_frame_once(hwnd, std::time::Duration::from_secs(2)) }?;
    let captured_handle = captured.shared_frame.shared_handle;
    let captured_dx12 = DxgiSharedHandleBridge.bridge_shared_handle(captured.shared_frame)?;
    let captured_surface_frame = captured_dx12.into_surface_frame();
    let WryWebSurfaceFrame::Native(captured_native_frame) = captured_surface_frame else {
        return Err("captured window bridge did not produce a native frame".into());
    };
    let captured_imported =
        importer.import_frame(&captured_native_frame, &ImportOptions::default())?;
    println!(
        "GraphicsCapture window probe: captured {}x{}, imported {:?} {}x{} generation {}",
        captured.content_size.width,
        captured.content_size.height,
        captured_imported.format,
        captured_imported.size.width,
        captured_imported.size.height,
        captured_imported.generation
    );
    unsafe {
        close_shared_handle(captured_handle)?;
    }

    let controller = webview.controller();
    let mut webview_hwnd = unsafe { std::mem::zeroed() };
    unsafe {
        controller.ParentWindow(&mut webview_hwnd)?;
    }
    match unsafe {
        capture_window_frame_once(
            webview_hwnd.0 as *mut std::ffi::c_void,
            std::time::Duration::from_secs(2),
        )
    } {
        Ok(captured) => {
            let captured_handle = captured.shared_frame.shared_handle;
            let captured_dx12 =
                DxgiSharedHandleBridge.bridge_shared_handle(captured.shared_frame)?;
            let captured_surface_frame = captured_dx12.into_surface_frame();
            let WryWebSurfaceFrame::Native(captured_native_frame) = captured_surface_frame else {
                return Err("captured Wry WebView2 bridge did not produce a native frame".into());
            };
            let captured_imported =
                importer.import_frame(&captured_native_frame, &ImportOptions::default())?;
            println!(
                "GraphicsCapture Wry WebView2 probe: captured child HWND {webview_hwnd:?} {}x{}, imported {:?} {}x{} generation {}",
                captured.content_size.width,
                captured.content_size.height,
                captured_imported.format,
                captured_imported.size.width,
                captured_imported.size.height,
                captured_imported.generation
            );
            unsafe {
                close_shared_handle(captured_handle)?;
            }
        }
        Err(error) => {
            println!(
                "GraphicsCapture Wry WebView2 child HWND probe: capture failed for {webview_hwnd:?}: {error}"
            );
        }
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn hwnd_from_window(window: &Window) -> Result<*mut std::ffi::c_void, Box<dyn std::error::Error>> {
    let handle = window.window_handle()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as *mut std::ffi::c_void),
        other => Err(format!("expected Win32 raw window handle, got {other:?}").into()),
    }
}

#[cfg(target_os = "windows")]
struct CapturedComposition {
    /// The most recently imported WebView texture; `None` until the renderer's
    /// first `try_acquire_frame` lands a frame. The probe path no longer
    /// blocks on an initial acquire (it was an intermittent pump-hang).
    imported: Option<wgpu_native_texture_interop::ImportedTexture>,
    producer: wry_wgpu_interop_adapter::webview2_composition_producer::WebView2CompositionProducer,
    #[allow(dead_code)]
    dispatcher_queue: Option<windows::System::DispatcherQueueController>,
}

#[cfg(target_os = "windows")]
fn run_webview2_composition_visual_probe(
    window: &Window,
    host: &HostWgpuContext,
) -> Result<Option<CapturedComposition>, Box<dyn std::error::Error>> {
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
        DispatcherQueueOptions,
    };
    use wry_wgpu_interop_adapter::webview2_composition_producer::{
        WebView2CompositionConfig, WebView2CompositionProducer,
    };
    use wry_wgpu_interop_adapter::windows_capture::close_shared_handle;

    let parent_hwnd = hwnd_from_window(window)?;
    let dispatcher_queue = match unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
    } {
        Ok(controller) => Some(controller),
        Err(error) => {
            println!(
                "CompositionController visual probe: dispatcher queue setup returned {error}; continuing"
            );
            None
        }
    };

    let user_data_dir = std::env::temp_dir().join("demo-wry-winit-composition-controller-webview2");
    let config = WebView2CompositionConfig::new(
        winit::dpi::PhysicalSize::new(COMPOSITION_PROBE_WIDTH as u32, COMPOSITION_PROBE_HEIGHT as u32),
        user_data_dir,
    )
    .with_offset(COMPOSITION_PROBE_X, COMPOSITION_PROBE_Y)
    .with_diagnostic_backdrop((27, 86, 96));

    let producer = unsafe { WebView2CompositionProducer::new(parent_hwnd, config)? };
    producer.navigate_to_string(COMPOSITION_WEBVIEW_PROBE_HTML, std::time::Duration::from_secs(5))?;
    println!("CompositionController visual probe: navigation completed");

    let imported = if std::env::var("WEBVIEW_READBACK_VALIDATE")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
    {
        let mut producer_for_readback = producer;
        let captured = producer_for_readback.acquire_full_frame()?;
        let importer = WgpuTextureImporter::new(host.clone());
        let WryWebSurfaceFrame::Native(ref native_frame) = captured.frame else {
            return Err("WebView2 composition producer did not emit a native frame".into());
        };
        let imported = importer.import_frame(native_frame, &ImportOptions::default())?;
        println!(
            "GraphicsCapture WebView2 CompositionController WebView target visual: captured {}x{}, imported {:?} {}x{} generation {}",
            captured.content_size.width,
            captured.content_size.height,
            imported.format,
            imported.size.width,
            imported.size.height,
            imported.generation
        );
        let html_background_rgb = (0x17u8, 0x20u8, 0x2au8);
        validate_imported_pixels(&imported, &host.device, &host.queue, html_background_rgb)?;
        unsafe {
            close_shared_handle(captured.shared_handle)?;
        }
        return Ok(Some(CapturedComposition {
            imported: Some(imported),
            producer: producer_for_readback,
            dispatcher_queue,
        }));
    } else {
        eprintln!(
            "WebView readback: skipped (set WEBVIEW_READBACK_VALIDATE=1 to enable startup pixel check + initial acquire). Renderer will perform first acquire on its own."
        );
        None
    };

    Ok(Some(CapturedComposition {
        imported,
        producer,
        dispatcher_queue,
    }))
}

#[cfg(target_os = "windows")]
fn validate_imported_pixels(
    imported: &wgpu_native_texture_interop::ImportedTexture,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    expected_rgb: (u8, u8, u8),
) -> Result<(), Box<dyn std::error::Error>> {
    if imported.format != wgpu::TextureFormat::Bgra8Unorm {
        return Err(format!(
            "WebView readback: expected Bgra8Unorm imported texture, got {:?}",
            imported.format
        )
        .into());
    }

    let width = imported.size.width;
    let height = imported.size.height;
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = (padded_bytes_per_row as u64) * (height as u64);

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("webview-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("webview-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &imported.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()
        .map_err(|error| format!("readback channel closed: {error}"))?
        .map_err(|error| format!("buffer map failed: {error}"))?;
    let data = slice.get_mapped_range();

    let row_stride = padded_bytes_per_row as usize;
    let sample = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y as usize) * row_stride + (x as usize) * 4;
        [data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]
    };

    let inset = 4u32.min(width.saturating_sub(1)).min(height.saturating_sub(1));
    let tl = sample(inset, inset);
    let tr = sample(width.saturating_sub(1 + inset), inset);
    let bl = sample(inset, height.saturating_sub(1 + inset));
    let br = sample(width.saturating_sub(1 + inset), height.saturating_sub(1 + inset));
    let center = sample(width / 2, height / 2);

    drop(data);
    buffer.unmap();

    let (er, eg, eb) = expected_rgb;
    println!(
        "WebView readback: expected background BGRA=({},{},{},255) from CSS rgb({},{},{})",
        eb, eg, er, er, eg, eb
    );
    println!(
        "WebView readback: tl=BGRA{:?} tr=BGRA{:?} bl=BGRA{:?} br=BGRA{:?} center=BGRA{:?}",
        tl, tr, bl, br, center
    );

    let tolerance: i32 = 6;
    let close_to_background = |bgra: [u8; 4]| -> bool {
        let [b, g, r, _a] = bgra;
        (b as i32 - eb as i32).abs() <= tolerance
            && (g as i32 - eg as i32).abs() <= tolerance
            && (r as i32 - er as i32).abs() <= tolerance
    };
    let corners_match =
        close_to_background(tl) && close_to_background(tr) && close_to_background(bl) && close_to_background(br);
    println!(
        "WebView readback: corner pixels match background within ±{tolerance}: {corners_match}"
    );

    if !corners_match {
        return Err(
            "WebView readback: corner pixels do not match the HTML background; \
             capture content is likely wrong (zeros, swapped channels, or empty)."
                .into(),
        );
    }

    let nonzero_alpha = tl[3] > 0 || tr[3] > 0 || bl[3] > 0 || br[3] > 0 || center[3] > 0;
    if !nonzero_alpha {
        return Err(
            "WebView readback: every sampled alpha is zero; capture is likely uninitialized.".into(),
        );
    }

    Ok(())
}

async fn create_host_device() -> Result<
    (wgpu::Instance, wgpu::Device, wgpu::Queue, wgpu::AdapterInfo),
    Box<dyn std::error::Error>,
> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: preferred_backends(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .map_err(|error| format!("adapter request failed: {error}"))?;

    let adapter_info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("demo-wry-winit"),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("device request failed: {error}"))?;

    Ok((instance, device, queue, adapter_info))
}

fn preferred_backends() -> wgpu::Backends {
    if cfg!(target_os = "windows") {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::PRIMARY
    }
}

fn frame_label(frame: &WryWebSurfaceFrame) -> &'static str {
    match frame.mode() {
        WebSurfaceMode::ImportedTexture => "imported texture",
        WebSurfaceMode::NativeChildOverlay => "native child overlay",
        WebSurfaceMode::CpuSnapshot => "CPU snapshot",
        WebSurfaceMode::Unsupported => "unsupported",
        _ => "unknown",
    }
}

#[cfg(target_os = "windows")]
const WEBVIEW_BLIT_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

@group(0) @binding(0) var captured: texture_2d<f32>;
@group(0) @binding(1) var captured_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(captured, captured_sampler, in.uv);
}
"#;

#[cfg(target_os = "windows")]
struct WebViewRenderer {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    importer: WgpuTextureImporter,
    captured: CapturedComposition,
    /// Tiny destination buffer for a 1×1 `copy_texture_to_buffer` issued
    /// each render. The copy itself is throwaway — the point is to force
    /// wgpu to emit a `SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE`
    /// transition barrier on the imported texture, which on D3D12 flushes
    /// shader caches that would otherwise hold the producer's first
    /// captured frame indefinitely. Without this the wgpu render goes
    /// stale even while the producer continuously `CopyResource`s into
    /// the same shared D3D11 texture.
    cache_flush_buffer: wgpu::Buffer,
    frames_imported: u64,
    frames_polled: u64,
    frames_acquired: u64,
    frames_resource_swapped: u64,
    consecutive_empty_polls: u32,
    capture_restarts: u64,
    last_metric_log: std::time::Instant,
    /// Pending producer resize, deferred until the user stops dragging.
    /// Stores the most recent target window size and the time of the last
    /// `Resized` event. We apply the producer rebuild only after a quiet
    /// period — `producer.resize` + `start_capture` together cost ~300ms,
    /// which would wedge the Win32 modal resize loop if run synchronously
    /// per-event during a drag.
    pending_producer_resize: Option<(winit::dpi::PhysicalSize<u32>, std::time::Instant)>,
    last_committed_capture_size: winit::dpi::PhysicalSize<u32>,
    /// Last (width, height) actually passed to `surface.configure`.
    /// Tracked separately from `surface_config.width/height` so we can
    /// notice when the resize handler updated the desired size and lazily
    /// reconfigure on the next render.
    configured_surface_size: (u32, u32),
}

#[cfg(target_os = "windows")]
impl WebViewRenderer {
    fn new(
        instance: wgpu::Instance,
        window: Arc<Window>,
        host: HostWgpuContext,
        captured: CapturedComposition,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let device = host.device.clone();
        let queue = host.queue.clone();
        let importer = WgpuTextureImporter::new(host.clone());
        let surface = instance.create_surface(window.clone())?;
        let size = window.inner_size();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|error| format!("renderer adapter request failed: {error}"))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm))
            .unwrap_or_else(|| caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("webview-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(WEBVIEW_BLIT_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("webview-blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("webview-blit-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("webview-blit-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("webview-blit-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // 1×1 placeholder so the bind group has something to point at until
        // the first real WebView frame lands via try_acquire_frame.
        let placeholder_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webview-blit-placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bind_group = match captured.imported.as_ref() {
            Some(imported) => {
                build_bind_group(&device, &bind_group_layout, &sampler, imported)
            }
            None => build_bind_group_for_texture(
                &device,
                &bind_group_layout,
                &sampler,
                &placeholder_texture,
            ),
        };

        // 256 bytes is `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`; minimum size
        // that satisfies `copy_texture_to_buffer` row-stride rules even
        // for a 1×1 copy. We never read it.
        let cache_flush_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("webview-cache-flush-buffer"),
            size: wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            window,
            device,
            queue,
            surface,
            surface_config,
            pipeline,
            bind_group_layout,
            sampler,
            bind_group,
            importer,
            captured,
            cache_flush_buffer,
            frames_imported: 1,
            frames_polled: 0,
            frames_acquired: 0,
            frames_resource_swapped: 0,
            consecutive_empty_polls: 0,
            capture_restarts: 0,
            last_metric_log: std::time::Instant::now(),
            pending_producer_resize: None,
            last_committed_capture_size: capture_size_for_window(size),
            configured_surface_size: (size.width.max(1), size.height.max(1)),
        })
    }

    /// Poll the producer for a fresh capture frame.
    ///
    /// In steady state the producer reuses a single shared D3D11 destination
    /// texture, so most polls return `resource_is_new = false` — the bind
    /// group's `wgpu::Texture` already references the same memory and just
    /// needs to be re-rendered. Only when the producer (re-)allocates (first
    /// frame, post-resize) do we re-import and rebuild the bind group.
    fn refresh_captured_texture(&mut self) -> Result<bool, Box<dyn std::error::Error>> {
        use wry_wgpu_interop_adapter::WryWebSurfaceFrame;
        use wry_wgpu_interop_adapter::windows_capture::close_shared_handle;

        // Diagnostic: if FORCE_REIMPORT_EVERY_FRAME=1 in the env, drop the
        // producer's persistent dest before each acquire so every frame goes
        // through the full re-import path (fresh NT handle + new wgpu::Texture
        // + new bind group). This isolates whether the visible-frozen-wgpu
        // bug is a D3D11/D3D12 shared-texture cache coherence issue.
        if force_reimport_every_frame() {
            self.captured.producer.invalidate_persistent_dest();
        }

        self.frames_polled = self.frames_polled.saturating_add(1);

        let new_frame = match self.captured.producer.try_acquire_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.consecutive_empty_polls = self.consecutive_empty_polls.saturating_add(1);
                // ~120 polls / ~60Hz redraw ≈ 1s of no frames; assume the WGC
                // session is wedged after a resize and restart it.
                if self.consecutive_empty_polls >= 120 {
                    self.captured.producer.force_restart_capture();
                    self.capture_restarts = self.capture_restarts.saturating_add(1);
                    self.consecutive_empty_polls = 0;
                    eprintln!(
                        "demo-wry-winit: capture stalled after >=1s of empty polls; restarting capture session (restart #{})",
                        self.capture_restarts
                    );
                }
                self.maybe_log_metrics();
                return Ok(false);
            }
            Err(error) => {
                eprintln!("demo-wry-winit: try_acquire_frame failed: {error}");
                return Ok(false);
            }
        };

        self.frames_acquired = self.frames_acquired.saturating_add(1);
        self.consecutive_empty_polls = 0;

        if new_frame.resource_is_new {
            let WryWebSurfaceFrame::Native(ref native_frame) = new_frame.frame else {
                return Err("WebView2 producer did not emit a native frame".into());
            };
            let imported = self
                .importer
                .import_frame(native_frame, &ImportOptions::default())?;
            unsafe {
                close_shared_handle(new_frame.shared_handle)?;
            }

            self.bind_group = build_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.sampler,
                &imported,
            );
            self.captured.imported = Some(imported);
            self.frames_imported = self.frames_imported.saturating_add(1);
            self.frames_resource_swapped = self.frames_resource_swapped.saturating_add(1);
        }

        self.maybe_log_metrics();
        Ok(true)
    }

    fn maybe_log_metrics(&mut self) {
        let elapsed = self.last_metric_log.elapsed();
        if elapsed < std::time::Duration::from_secs(2) {
            return;
        }
        let secs = elapsed.as_secs_f64().max(0.001);
        println!(
            "renderer metrics ({:.1}s): polled={}, acquired={} ({:.1}/s), resource_swaps={}, total_imports={}, capture_restarts={}",
            secs,
            self.frames_polled,
            self.frames_acquired,
            (self.frames_acquired as f64) / secs,
            self.frames_resource_swapped,
            self.frames_imported,
            self.capture_restarts,
        );
        self.frames_polled = 0;
        self.frames_acquired = 0;
        self.frames_resource_swapped = 0;
        self.last_metric_log = std::time::Instant::now();
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        // Truly O(1): only stash state. Even `surface.configure` is too
        // expensive to call from inside the Win32 modal resize loop —
        // winit fires WindowEvent::Resized at very high cadence during a
        // drag and a few-ms-per-call configure starves the modal loop's
        // input processing, locking the cursor in resize-arrow mode.
        // Both surface reconfigure and producer rebuild happen lazily
        // from `render()`.
        self.surface_config.width = new_size.width;
        self.surface_config.height = new_size.height;
        self.pending_producer_resize = Some((new_size, std::time::Instant::now()));
    }

    /// Apply a deferred producer resize once the user has stopped dragging.
    /// Returns true if a producer rebuild actually ran.
    fn apply_pending_resize(&mut self) -> bool {
        const SETTLE_MS: u128 = 120;
        let (target_size, last_event) = match self.pending_producer_resize {
            Some(p) => p,
            None => return false,
        };
        let elapsed = last_event.elapsed().as_millis();
        if elapsed < SETTLE_MS {
            return false;
        }
        let capture_size = capture_size_for_window(target_size);
        if capture_size == self.last_committed_capture_size {
            self.pending_producer_resize = None;
            return false;
        }
        let (offset_x, offset_y) = capture_offset_for_window(target_size);
        println!(
            "resize (settled): window={}x{} -> capture={}x{} offset=({}, {})",
            target_size.width,
            target_size.height,
            capture_size.width,
            capture_size.height,
            offset_x,
            offset_y
        );
        let _ = elapsed;
        if let Err(error) = self.captured.producer.set_offset(offset_x, offset_y) {
            eprintln!(
                "demo-wry-winit: producer.set_offset({offset_x}, {offset_y}) failed: {error}"
            );
        }
        if let Err(error) = self.captured.producer.resize(capture_size) {
            eprintln!(
                "demo-wry-winit: producer.resize({}x{}) failed: {error}",
                capture_size.width, capture_size.height
            );
        }
        self.last_committed_capture_size = capture_size;
        self.pending_producer_resize = None;
        true
    }

    fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Lazy surface reconfigure, kept out of the resize event handler so
        // it can't run inside the Win32 modal resize loop.
        let desired = (self.surface_config.width, self.surface_config.height);
        if desired != self.configured_surface_size {
            self.surface.configure(&self.device, &self.surface_config);
            self.configured_surface_size = desired;
        }
        self.apply_pending_resize();
        let _ = self.refresh_captured_texture()?;
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webview-blit-encoder"),
            });

        // Force wgpu to insert a SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE
        // transition on the imported texture by issuing a throwaway 1×1 copy
        // before the render pass samples it. On D3D12 the transition flushes
        // the shader caches that would otherwise hold a stale view of the
        // externally-written shared NT-handle texture.
        if let Some(imported) = self.captured.imported.as_ref() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &imported.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.cache_flush_buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webview-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        frame.present();
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn build_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    imported: &wgpu_native_texture_interop::ImportedTexture,
) -> wgpu::BindGroup {
    build_bind_group_for_texture(device, layout, sampler, &imported.texture)
}

#[cfg(target_os = "windows")]
fn build_bind_group_for_texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture: &wgpu::Texture,
) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("webview-blit-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Half the window's width and full height for the WebView capture target.
/// Pairs with the demo's right-half overlay layout: the live composition
/// visual sits on the right, and the wgpu surface fills the whole window
/// behind it with the captured texture stretched, so resize keeps the two
/// in sync.
#[cfg(target_os = "windows")]
fn capture_size_for_window(
    window_size: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    let w = (window_size.width / 2).max(120);
    let h = window_size.height.max(120);
    winit::dpi::PhysicalSize::new(w, h)
}

/// Top-left of the WinComp overlay relative to the parent window. Pairs with
/// `capture_size_for_window`: pin the right-half overlay flush against the
/// window's right edge so it tracks the intended layout as the window resizes.
#[cfg(target_os = "windows")]
fn force_reimport_every_frame() -> bool {
    std::env::var("FORCE_REIMPORT_EVERY_FRAME")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
}

#[cfg(target_os = "windows")]
fn capture_offset_for_window(window_size: winit::dpi::PhysicalSize<u32>) -> (f32, f32) {
    let capture = capture_size_for_window(window_size);
    let x = window_size.width.saturating_sub(capture.width) as f32;
    (x, 0.0)
}
