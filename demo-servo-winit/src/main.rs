//! Minimal winit + wgpu demo embedding Servo as a web renderer.
//!
//! Demonstrates both GPU texture import and CPU readback paths. On Windows with
//! Servo/ANGLE, the zero-copy path uses `eglQuerySurfacePointerANGLE` to obtain
//! the D3D11 shared handle and imports it via `VK_KHR_external_memory_win32`.
//! Falls back to `GL_EXT_memory_object_win32` (non-ANGLE Vulkan GL), then to
//! CPU readback (`read_full_frame()` → `write_texture()`) if no GPU path works.
//!
//! Mouse, scroll, and keyboard events are forwarded directly to Servo so
//! pages are fully interactive (links, scrolling, text input).
//!
//! This is the "bare-minimum" embedding demo — no UI toolkit, no URL bar.
//! Pass URLs via the command line. The current URL is shown in the title bar.
//! For a demo with a URL bar and navigation UI, see `demo-servo-xilem`.
//!
//! Usage:
//!   cargo run -p demo-servo-winit -- https://example.com
//!   cargo run -p demo-servo-winit -- servo.org        # auto-prefixes https://
//!   cargo run -p demo-servo-winit                     # opens built-in fixture page

use std::{borrow::Cow, path::PathBuf, rc::Rc, sync::Arc};

use euclid::Scale;
use rustls::crypto::aws_lc_rs;
use servo::{
    DevicePoint, EventLoopWaker, InputEvent, MouseButton as ServoMouseButton, MouseButtonAction,
    MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, Servo,
    ServoBuilder, WebView, WebViewBuilder, WebViewDelegate, WheelDelta, WheelEvent, WheelMode,
};
use servo_wgpu_interop_adapter::ServoWgpuInteropAdapter;
use url::Url;
use wgpu::SurfaceError;
use wgpu_native_texture_interop::{HostWgpuContext, InteropBackend};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::Window,
};

mod keyutils;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls aws-lc provider");

    let event_loop = EventLoop::with_user_event()
        .build()
        .expect("failed to create event loop");
    let initial_url = resolve_initial_url()?;
    let mut app = App::new(&event_loop, initial_url);
    Ok(event_loop.run_app(&mut app)?)
}

struct App {
    state: AppStage,
}

enum AppStage {
    Initial { initial_url: Url, waker: AppWaker },
    Running(AppState),
}

struct AppState {
    window: Arc<Window>,
    servo: Servo,
    webview: WebView,
    interop: ServoWgpuInteropAdapter,
    renderer: Renderer,
    gpu_import_failed: bool,
    // Input state
    cursor_position: PhysicalPosition<f64>,
    modifiers: ModifiersState,
    scale_factor: f64,
}

impl App {
    fn new(event_loop: &EventLoop<WakerEvent>, initial_url: Url) -> Self {
        Self {
            state: AppStage::Initial {
                initial_url,
                waker: AppWaker::new(event_loop),
            },
        }
    }
}

impl ApplicationHandler<WakerEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let AppStage::Initial { initial_url, waker } = &self.state else {
            return;
        };

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("demo-servo-winit")
                        .with_inner_size(PhysicalSize::new(1280, 800)),
                )
                .expect("failed to create window"),
        );

        let renderer =
            pollster::block_on(Renderer::new(window.clone())).expect("failed to create renderer");
        let size = window.inner_size();
        let scale_factor = window.scale_factor();

        let interop = ServoWgpuInteropAdapter::new(
            renderer.device.clone(),
            renderer.queue.clone(),
            size,
        )
        .expect("failed to create Servo interop adapter");

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        servo.setup_logging();

        let delegate = Rc::new(RedrawDelegate {
            window: window.clone(),
        });

        let webview = WebViewBuilder::new(&servo, interop.rendering_context())
            .url(initial_url.clone())
            .hidpi_scale_factor(Scale::new(scale_factor as f32))
            .delegate(delegate)
            .build();

        log_startup_diagnostics(initial_url, &renderer, &interop);
        window.request_redraw();

        self.state = AppStage::Running(AppState {
            window,
            servo,
            webview,
            interop,
            renderer,
            gpu_import_failed: false,
            cursor_position: PhysicalPosition::new(0.0, 0.0),
            modifiers: ModifiersState::default(),
            scale_factor,
        });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: WakerEvent) {
        if let AppStage::Running(state) = &mut self.state {
            state.servo.spin_event_loop();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let AppStage::Running(state) = &mut self.state else {
            return;
        };

        state.servo.spin_event_loop();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::RedrawRequested => {
                if let Err(error) = state.render_frame() {
                    eprintln!("render failed: {error}");
                    event_loop.exit();
                }
            }

            WindowEvent::Resized(new_size) => {
                state.renderer.resize(new_size);
                state.interop.rendering_context_handle().resize_viewport(new_size);
                state.webview.resize(new_size);
                state.window.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.scale_factor = scale_factor;
                state
                    .webview
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                state.window.request_redraw();
            }

            WindowEvent::ModifiersChanged(mods) => {
                state.modifiers = mods.state();
            }

            WindowEvent::CursorMoved { position, .. } => {
                state.cursor_position = position;
                let point = DevicePoint::new(position.x as f32, position.y as f32);
                state.webview.notify_input_event(InputEvent::MouseMove(
                    MouseMoveEvent::new(servo::WebViewPoint::Device(point)),
                ));
            }

            WindowEvent::CursorLeft { .. } => {
                state
                    .webview
                    .notify_input_event(InputEvent::MouseLeftViewport(
                        MouseLeftViewportEvent::default(),
                    ));
            }

            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                let servo_button = match button {
                    winit::event::MouseButton::Left => ServoMouseButton::Left,
                    winit::event::MouseButton::Right => ServoMouseButton::Right,
                    winit::event::MouseButton::Middle => ServoMouseButton::Middle,
                    _ => return,
                };
                let action = match btn_state {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                let pos = state.cursor_position;
                let point = DevicePoint::new(pos.x as f32, pos.y as f32);
                state.webview.notify_input_event(InputEvent::MouseButton(
                    MouseButtonEvent::new(
                        action,
                        servo_button,
                        servo::WebViewPoint::Device(point),
                    ),
                ));
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        ((x as f64) * 38.0, (y as f64) * 38.0, WheelMode::DeltaLine)
                    }
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.x, pos.y, WheelMode::DeltaPixel)
                    }
                };
                let pos = state.cursor_position;
                let point = DevicePoint::new(pos.x as f32, pos.y as f32);
                state.webview.notify_input_event(InputEvent::Wheel(
                    WheelEvent::new(
                        WheelDelta { x: dx, y: dy, z: 0.0, mode },
                        servo::WebViewPoint::Device(point),
                    ),
                ));
            }

            WindowEvent::KeyboardInput { event, .. } => {
                let kbd = keyutils::keyboard_event_from_winit(&event, state.modifiers);
                state
                    .webview
                    .notify_input_event(InputEvent::Keyboard(kbd));
            }

            _ => {}
        }
    }
}

impl AppState {
    fn render_frame(&mut self) -> Result<(), String> {
        self.webview.paint();

        // GPU path: import the GL framebuffer directly as a wgpu texture.
        // Falls back to CPU readback if the GL driver lacks external memory extensions.
        if !self.gpu_import_failed {
            match self.interop.import_current_frame_default() {
                Ok(imported) => {
                    return self
                        .renderer
                        .render_texture(&imported.texture)
                        .map_err(|e| format!("surface render error: {e}"));
                }
                Err(e) => {
                    eprintln!("[demo] GPU import unavailable, falling back to CPU readback: {e}");
                    self.gpu_import_failed = true;
                }
            }
        }

        // CPU fallback: read pixels from GL, upload via write_texture.
        if let Some(image) = self.interop.rendering_context_handle().read_full_frame() {
            self.renderer.upload_frame(&image);
        }
        self.renderer
            .render_cached()
            .map_err(|e| format!("surface render error: {e}"))
    }
}

// ── Renderer ────────────────────────────────────────────────────────────────

struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    host_backend: InteropBackend,
    // CPU fallback: cached frame texture uploaded via write_texture.
    frame_texture: Option<wgpu::Texture>,
    frame_bind_group: Option<wgpu::BindGroup>,
    frame_size: PhysicalSize<u32>,
}

impl Renderer {
    async fn new(window: Arc<Window>) -> Result<Self, String> {
        // On Windows, prefer Vulkan so the ANGLE D3D11 share handle import path works.
        // Allow DX12 as a fallback for systems without a Vulkan driver.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            #[cfg(target_os = "windows")]
            backends: wgpu::Backends::VULKAN | wgpu::Backends::DX12,
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| error.to_string())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .map_err(|error| error.to_string())?;

        // Request VULKAN_EXTERNAL_MEMORY_WIN32 if the adapter supports it.
        // This is required for the ANGLE D3D11 share handle zero-copy import path.
        // If unsupported, we fall back to the CPU readback path transparently.
        #[cfg(target_os = "windows")]
        let extra_features = adapter.features()
            & wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32;
        #[cfg(not(target_os = "windows"))]
        let extra_features = wgpu::Features::empty();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("demo-servo-winit-device"),
                required_features: extra_features,
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|error| error.to_string())?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(surface_caps.formats[0]);

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame-texture-layout"),
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fullscreen-quad-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(FULLSCREEN_QUAD_WGSL)),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frame-pipeline-layout"),
            bind_group_layouts: &[&texture_bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("frame-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let host_backend = HostWgpuContext::new(device.clone(), queue.clone()).backend;

        Ok(Self {
            surface,
            device,
            queue,
            config,
            texture_bind_group_layout,
            sampler,
            pipeline,
            host_backend,
            frame_texture: None,
            frame_bind_group: None,
            frame_size: PhysicalSize::new(0, 0),
        })
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Render a GPU-imported wgpu texture (zero-copy path).
    fn render_texture(&self, texture: &wgpu::Texture) -> Result<(), SurfaceError> {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu-frame-bind-group"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.draw_fullscreen_quad(Some(&bind_group))
    }

    /// Upload a CPU-side RGBA image as the cached frame texture.
    fn upload_frame(&mut self, image: &image::RgbaImage) {
        let (w, h) = image.dimensions();
        let new_size = PhysicalSize::new(w, h);

        if self.frame_size != new_size {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("servo-frame-cpu"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });

            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cpu-frame-bind-group"),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            self.frame_texture = Some(texture);
            self.frame_bind_group = Some(bind_group);
            self.frame_size = new_size;
        }

        if let Some(texture) = &self.frame_texture {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.as_raw(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * w),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Render the cached CPU-uploaded frame texture.
    fn render_cached(&self) -> Result<(), SurfaceError> {
        self.draw_fullscreen_quad(self.frame_bind_group.as_ref())
    }

    fn draw_fullscreen_quad(&self, bind_group: Option<&wgpu::BindGroup>) -> Result<(), SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let surface_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render-encoder"),
            });

        let clear_color = if bind_group.is_some() {
            wgpu::Color::BLACK
        } else {
            wgpu::Color {
                r: 0.12,
                g: 0.05,
                b: 0.05,
                a: 1.0,
            }
        };

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if let Some(bind_group) = bind_group {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

// ── Fullscreen quad shader ──────────────────────────────────────────────────

const FULLSCREEN_QUAD_WGSL: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var output: VertexOut;
    output.position = vec4<f32>(positions[index], 0.0, 1.0);
    output.uv = uvs[index];
    return output;
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(source_texture, source_sampler, input.uv);
}
"#;

// ── Waker ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppWaker {
    proxy: EventLoopProxy<WakerEvent>,
}

#[derive(Debug)]
struct WakerEvent;

impl AppWaker {
    fn new(event_loop: &EventLoop<WakerEvent>) -> Self {
        Self {
            proxy: event_loop.create_proxy(),
        }
    }
}

impl EventLoopWaker for AppWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.proxy.send_event(WakerEvent);
    }
}

// ── WebView delegate ────────────────────────────────────────────────────────

struct RedrawDelegate {
    window: Arc<Window>,
}

impl WebViewDelegate for RedrawDelegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.window.request_redraw();
    }

    fn notify_url_changed(&self, _webview: WebView, url: Url) {
        self.window.set_title(&format!("demo-servo-winit — {url}"));
        println!("[servo] URL changed: {url}");
    }

    fn notify_closed(&self, _webview: WebView) {
        println!("[servo] webview closed");
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, backtrace: Option<String>) {
        eprintln!("[servo] CRASH: {reason}");
        if let Some(bt) = backtrace {
            eprintln!("{bt}");
        }
    }
}

// ── URL resolution ──────────────────────────────────────────────────────────

fn resolve_initial_url() -> Result<Url, String> {
    if let Some(argument) = std::env::args().nth(1) {
        return resolve_url_argument(&argument);
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest_dir.join("fixtures").join("animated.html");
    Url::from_file_path(&fixture).map_err(|_| {
        format!(
            "failed to convert fixture path to file URL: {}",
            fixture.display()
        )
    })
}

fn resolve_url_argument(argument: &str) -> Result<Url, String> {
    if let Ok(url) = Url::parse(argument) {
        return Ok(url);
    }

    if let Ok(url) = Url::parse(&format!("https://{argument}")) {
        return Ok(url);
    }

    let candidate = PathBuf::from(argument);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(candidate)
    };

    Url::from_file_path(&absolute)
        .map_err(|_| format!("argument was neither a URL nor a file path: {argument}"))
}

fn log_startup_diagnostics(
    initial_url: &Url,
    renderer: &Renderer,
    interop: &ServoWgpuInteropAdapter,
) {
    let capabilities = interop.importer().host().capabilities();
    println!("demo url: {initial_url}");
    println!("host backend: {:?}", renderer.host_backend);
    println!("capabilities: {capabilities:?}");
}
