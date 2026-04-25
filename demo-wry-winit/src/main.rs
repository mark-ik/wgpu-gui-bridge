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
    _device: wgpu::Device,
    _queue: wgpu::Queue,
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
        if let WindowEvent::CloseRequested = event {
            event_loop.exit();
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

        let (device, queue, adapter_info) = pollster::block_on(create_host_device())?;
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

        #[cfg(target_os = "windows")]
        run_windows_shared_texture_probe(&window, &host)?;

        Ok(Self {
            window,
            _device: device,
            _queue: queue,
        })
    }
}

#[cfg(target_os = "windows")]
fn run_windows_shared_texture_probe(
    window: &Window,
    host: &HostWgpuContext,
) -> Result<(), Box<dyn std::error::Error>> {
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

async fn create_host_device()
-> Result<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo), Box<dyn std::error::Error>> {
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

    Ok((device, queue, adapter_info))
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
