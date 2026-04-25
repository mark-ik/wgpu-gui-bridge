//! Minimal winit + wgpu host probe for Wry/system-webview texture interop.

use std::sync::Arc;

use wgpu_native_texture_interop::HostWgpuContext;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;
use wry_wgpu_interop_adapter::{
    OverlayOnlyProducer, WebSurfaceMode, WryWebSurfaceFrame, WryWebSurfaceProducer,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::default();
    Ok(event_loop.run_app(&mut app)?)
}

#[derive(Default)]
struct App {
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
            Ok(state) => self.state = Some(state),
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

        Ok(Self {
            window,
            _device: device,
            _queue: queue,
        })
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
