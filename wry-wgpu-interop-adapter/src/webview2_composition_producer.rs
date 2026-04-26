//! Windows-only WebView2 composition-controller capture producer.
//!
//! This owns the moving parts the demo previously inlined:
//! - WebView2 environment / composition controller / controller / webview
//! - Windows.UI.Composition compositor, desktop window target, root + webview
//!   visuals
//! - Windows.Graphics.Capture item / frame pool / session lifecycle
//! - Post-StartCapture content invalidation nudge for the first frame
//! - Shared-handle export for the host's `wgpu-native-texture-interop` importer
//!
//! The proven flow this encapsulates was validated as:
//! 1. Create a real WebView2 composition-controller WebView.
//! 2. Attach it to a WinComp container visual.
//! 3. Feed the visual to `GraphicsCaptureItem::CreateFromVisual`.
//! 4. Start WGC capture.
//! 5. Nudge WebView content after `StartCapture`.
//! 6. Receive a `Bgra8Unorm` frame.
//! 7. Bridge D3D11 capture output into a DX12-importable native frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2CompositionController, ICoreWebView2Controller,
    ICoreWebView2Environment, ICoreWebView2Environment3, ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    CoTaskMemPWSTR, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler,
    NavigationCompletedEventHandler,
};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};
use windows::core::{Interface, PCWSTR};
use windows_numerics::{Vector2, Vector3};

use crate::windows_capture::{
    D3D11SharedTexture, D3D11SharedTextureFactory, WebView2D3D11CaptureFrame,
    WebView2DxgiSharedHandleFrame,
};
use crate::{
    SystemWebviewBackend, WebSurfaceMode, WryWebSurfaceCapabilities, WryWebSurfaceError,
    WryWebSurfaceFrame,
};

const FIRST_FRAME_NUDGE_LABEL: &str = "WebView2CompositionProducer.first-frame";

/// Configuration for `WebView2CompositionProducer::new`.
#[derive(Clone, Debug)]
pub struct WebView2CompositionConfig {
    /// Initial size of the WebView visual and capture region.
    pub size: PhysicalSize<u32>,
    /// Offset of the root visual relative to the parent window.
    pub offset: (f32, f32),
    /// User-data directory for the WebView2 environment. Created if missing.
    pub user_data_dir: PathBuf,
    /// Optional CSS color used for a sprite visual placed under the WebView
    /// visual. Mostly useful as a diagnostic backstop while the WebView paints.
    pub diagnostic_backdrop: Option<(u8, u8, u8)>,
    /// Timeout for the navigation-completed wait inside `navigate_to_string`.
    pub navigation_timeout: Duration,
    /// Timeout for `acquire_frame` to wait on `TryGetNextFrame`.
    pub frame_timeout: Duration,
}

impl WebView2CompositionConfig {
    pub fn new(size: PhysicalSize<u32>, user_data_dir: impl Into<PathBuf>) -> Self {
        Self {
            size,
            offset: (0.0, 0.0),
            user_data_dir: user_data_dir.into(),
            diagnostic_backdrop: None,
            navigation_timeout: Duration::from_secs(5),
            frame_timeout: Duration::from_secs(2),
        }
    }

    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset = (x, y);
        self
    }

    pub fn with_diagnostic_backdrop(mut self, rgb: (u8, u8, u8)) -> Self {
        self.diagnostic_backdrop = Some(rgb);
        self
    }
}

/// Captured WebView frame ready to be imported via `wgpu-native-texture-interop`.
///
/// When `resource_is_new` is `true`, this frame points at a freshly allocated
/// shared D3D11 texture that the consumer must (re-)import; the consumer owns
/// the NT handle and is responsible for calling
/// `crate::windows_capture::close_shared_handle` after import.
///
/// When `resource_is_new` is `false`, the producer reused the previous
/// allocation: the consumer should keep its previously-imported `wgpu::Texture`
/// (whose underlying memory was just overwritten by the producer's
/// `CopyResource`) and ignore `shared_handle`.
pub struct WebView2CompositionFrame {
    pub frame: WryWebSurfaceFrame,
    pub content_size: PhysicalSize<u32>,
    pub generation: u64,
    pub shared_handle: *mut std::ffi::c_void,
    pub resource_is_new: bool,
}

/// WebView2 + WinComp + WGC capture producer.
///
/// Construction sets up the composition tree and the WebView2 environment.
/// Capture is started lazily on the first `acquire_frame` call so the caller
/// can navigate and prepare content first.
pub struct WebView2CompositionProducer {
    #[allow(dead_code)]
    parent_hwnd: HWND,
    size: PhysicalSize<u32>,
    generation: u64,

    #[allow(dead_code)]
    compositor: Compositor,
    #[allow(dead_code)]
    desktop_target: windows::UI::Composition::Desktop::DesktopWindowTarget,
    root_visual: ContainerVisual,
    webview_visual: ContainerVisual,

    #[allow(dead_code)]
    environment: ICoreWebView2Environment,
    #[allow(dead_code)]
    composition_controller: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,

    capture_factory: D3D11SharedTextureFactory,
    capture_device: IDirect3DDevice,
    capture_state: Option<CaptureState>,
    persistent_dest: Option<PersistentDest>,
}

/// A reusable shared D3D11 destination texture and its NT handle. The handle
/// is exposed exactly once via `WebView2CompositionFrame::shared_handle` (with
/// `resource_is_new = true`); subsequent frames reuse the same texture and
/// signal `resource_is_new = false`.
struct PersistentDest {
    texture: D3D11SharedTexture,
    size: PhysicalSize<u32>,
    handle_handed_off: bool,
}

struct CaptureState {
    #[allow(dead_code)]
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    first_frame_emitted: bool,
}

impl WebView2CompositionProducer {
    /// Build the composition tree, the WebView2 controller, and prepare for
    /// capture. Capture is not started until the first `acquire_frame` call.
    ///
    /// # Safety
    ///
    /// `parent_hwnd` must be a live top-level HWND for the lifetime of the
    /// returned producer.
    pub unsafe fn new(
        parent_hwnd: *mut std::ffi::c_void,
        config: WebView2CompositionConfig,
    ) -> Result<Self, WryWebSurfaceError> {
        if parent_hwnd.is_null() {
            return Err(WryWebSurfaceError::Platform(
                "parent HWND was null".to_string(),
            ));
        }
        if config.size.width == 0 || config.size.height == 0 {
            return Err(WryWebSurfaceError::Platform(format!(
                "WebView2 producer size must be non-zero, got {}x{}",
                config.size.width, config.size.height
            )));
        }

        let parent_hwnd = HWND(parent_hwnd);

        let compositor = Compositor::new().map_err(platform("Compositor::new"))?;
        let desktop_interop: ICompositorDesktopInterop =
            compositor.cast().map_err(platform("Compositor cast to ICompositorDesktopInterop"))?;
        let desktop_target = unsafe { desktop_interop.CreateDesktopWindowTarget(parent_hwnd, false) }
            .map_err(platform("CreateDesktopWindowTarget"))?;

        let root_visual = compositor
            .CreateContainerVisual()
            .map_err(platform("CreateContainerVisual (root)"))?;
        root_visual
            .SetOffset(Vector3 {
                X: config.offset.0,
                Y: config.offset.1,
                Z: 0.0,
            })
            .map_err(platform("ContainerVisual::SetOffset"))?;
        let visual_size = Vector2 {
            X: config.size.width as f32,
            Y: config.size.height as f32,
        };
        root_visual
            .SetSize(visual_size)
            .map_err(platform("ContainerVisual::SetSize (root)"))?;

        if let Some((r, g, b)) = config.diagnostic_backdrop {
            let sprite = compositor
                .CreateSpriteVisual()
                .map_err(platform("CreateSpriteVisual (diagnostic)"))?;
            sprite
                .SetSize(visual_size)
                .map_err(platform("SpriteVisual::SetSize"))?;
            let brush = compositor
                .CreateColorBrushWithColor(windows::UI::Color { A: 255, R: r, G: g, B: b })
                .map_err(platform("CreateColorBrushWithColor"))?;
            sprite
                .SetBrush(&brush)
                .map_err(platform("SpriteVisual::SetBrush"))?;
            root_visual
                .Children()
                .map_err(platform("root.Children()"))?
                .InsertAtBottom(&sprite)
                .map_err(platform("Children::InsertAtBottom"))?;
        }

        let webview_visual = compositor
            .CreateContainerVisual()
            .map_err(platform("CreateContainerVisual (webview)"))?;
        webview_visual
            .SetSize(visual_size)
            .map_err(platform("ContainerVisual::SetSize (webview)"))?;
        root_visual
            .Children()
            .map_err(platform("root.Children() (webview)"))?
            .InsertAtTop(&webview_visual)
            .map_err(platform("Children::InsertAtTop (webview)"))?;
        desktop_target
            .SetRoot(&root_visual)
            .map_err(platform("DesktopWindowTarget::SetRoot"))?;

        let environment = create_environment(&config.user_data_dir)?;
        let composition_controller =
            create_composition_controller(&environment, parent_hwnd)?;
        unsafe {
            composition_controller
                .SetRootVisualTarget(&webview_visual)
                .map_err(platform("SetRootVisualTarget"))?;
        }

        let controller: ICoreWebView2Controller = composition_controller
            .cast()
            .map_err(platform("composition controller cast"))?;
        unsafe {
            controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: config.size.width as i32,
                    bottom: config.size.height as i32,
                })
                .map_err(platform("controller.SetBounds"))?;
            controller
                .SetIsVisible(true)
                .map_err(platform("controller.SetIsVisible"))?;
        }
        let webview = unsafe { controller.CoreWebView2() }
            .map_err(platform("controller.CoreWebView2"))?;

        let capture_factory = D3D11SharedTextureFactory::new_hardware()?;
        let capture_device = capture_factory.create_winrt_direct3d_device()?;

        Ok(Self {
            parent_hwnd,
            size: config.size,
            generation: 0,
            compositor,
            desktop_target,
            root_visual,
            webview_visual,
            environment,
            composition_controller,
            controller,
            webview,
            capture_factory,
            capture_device,
            capture_state: None,
            persistent_dest: None,
        })
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    /// Navigate the underlying WebView2 to an inline HTML document and block
    /// until `NavigationCompleted` fires (or the configured timeout elapses).
    pub fn navigate_to_string(
        &self,
        html: &str,
        timeout: Duration,
    ) -> Result<(), WryWebSurfaceError> {
        let (tx, rx) = mpsc::channel::<()>();
        let mut navigation_token = 0;
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, _args| {
            let _ = tx.send(());
            Ok(())
        }));

        unsafe {
            self.webview
                .add_NavigationCompleted(&handler, &mut navigation_token)
                .map_err(platform("add_NavigationCompleted"))?;
            let html = CoTaskMemPWSTR::from(html);
            self.webview
                .NavigateToString(*html.as_ref().as_pcwstr())
                .map_err(platform("NavigateToString"))?;
        }

        let result = pump_until(timeout, &rx);

        unsafe {
            let _ = self
                .webview
                .remove_NavigationCompleted(navigation_token)
                .map_err(webview2_com::Error::WindowsError);
        }

        result.map_err(|()| {
            WryWebSurfaceError::Platform(format!(
                "WebView2 navigation did not complete within {timeout:?}"
            ))
        })?;

        // Make sure at least one render tick has happened so the visual has
        // content before capture starts.
        self.wait_for_render_tick()
    }

    fn wait_for_render_tick(&self) -> Result<(), WryWebSurfaceError> {
        let script = r#"(() => new Promise(resolve => {
            requestAnimationFrame(() => requestAnimationFrame(() => resolve("present")));
        }))()"#
            .to_string();
        execute_script_blocking(&self.webview, script)
    }

    /// Tear down the capture session + frame pool. The next call to
    /// `try_acquire_frame` will run `start_capture` against the current visual
    /// state, allocating a fresh `GraphicsCaptureItem`.
    ///
    /// Use this when the consumer detects that frame emission has stalled
    /// (e.g. enough consecutive `Ok(None)` polls to suggest WGC has lost track
    /// of the visual after rapid resize cycling). Persistent destination state
    /// is intentionally preserved — the consumer keeps its imported texture
    /// and only re-imports if `ContentSize` changes.
    pub fn force_restart_capture(&mut self) {
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
        }
    }

    /// Drop the cached shared D3D11 destination texture so the next
    /// `try_acquire_frame` allocates a fresh one and signals
    /// `resource_is_new = true`.
    ///
    /// This is the consumer-driven escape hatch for D3D12-side cache
    /// staleness on the externally-written shared texture: by forcing a
    /// re-import (new `ID3D12Resource` from a fresh NT handle, new
    /// `wgpu::Texture` and bind group), the consumer flushes whatever
    /// driver-level caching was holding the previous frame's pixels.
    pub fn invalidate_persistent_dest(&mut self) {
        self.persistent_dest = None;
    }

    /// Reposition the root visual relative to the parent window, in physical
    /// pixels. The capture region follows the visual.
    pub fn set_offset(&self, x: f32, y: f32) -> Result<(), WryWebSurfaceError> {
        self.root_visual
            .SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })
            .map_err(platform("root.SetOffset"))
    }

    /// Resize the WebView visual, controller bounds, and capture frame pool.
    pub fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WryWebSurfaceError> {
        if size.width == 0 || size.height == 0 {
            return Err(WryWebSurfaceError::Platform(format!(
                "WebView2 producer resize must be non-zero, got {}x{}",
                size.width, size.height
            )));
        }
        if size == self.size {
            return Ok(());
        }
        eprintln!(
            "[producer] resize: {}x{} -> {}x{}",
            self.size.width, self.size.height, size.width, size.height
        );

        let visual_size = Vector2 {
            X: size.width as f32,
            Y: size.height as f32,
        };
        self.root_visual
            .SetSize(visual_size)
            .map_err(platform("root.SetSize"))?;
        self.webview_visual
            .SetSize(visual_size)
            .map_err(platform("webview_visual.SetSize"))?;
        unsafe {
            self.controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: size.width as i32,
                    bottom: size.height as i32,
                })
                .map_err(platform("controller.SetBounds"))?;
        }

        // `Direct3D11CaptureFramePool::Recreate` does not reliably resume frame
        // emission against a resized WinComp visual: in practice it produces
        // exactly one frame at the new size and then goes silent. Tear the
        // session + pool down here so the next `try_acquire_frame` calls
        // `start_capture()` against a fresh `GraphicsCaptureItem` derived from
        // the resized visual, with a fresh frame budget and a re-armed nudge.
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
        }

        // Drop the persistent destination so the next capture allocates a
        // freshly-sized texture and re-issues a shared NT handle. The consumer
        // sees `resource_is_new = true` and can re-import on its side.
        self.persistent_dest = None;

        self.size = size;
        Ok(())
    }

    /// Acquire the next capture frame, returning the full producer-side
    /// frame (including the platform-specific shared NT handle and the
    /// `resource_is_new` reuse hint).
    ///
    /// The first call lazily starts the capture session and runs a
    /// one-shot content nudge so WebView2 issues a fresh paint that WGC
    /// will observe.
    ///
    /// Generic consumers can use [`Self::acquire_frame`] (the
    /// `WryWebSurfaceProducer` trait method) for the platform-agnostic
    /// view of the same frame.
    pub fn acquire_full_frame(
        &mut self,
    ) -> Result<WebView2CompositionFrame, WryWebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }
        let timeout = Duration::from_secs(2);
        self.acquire_frame_with_timeout(timeout)
    }

    /// Non-blocking variant of `acquire_frame`: poll the frame pool exactly
    /// once. Returns `Ok(None)` when no new frame is ready, leaving the
    /// capture session running for the next call.
    ///
    /// This is the per-render-frame entry point in steady-state: call it
    /// every redraw, swap the consumer's bound texture only when `Some` is
    /// returned, and otherwise reuse the previous frame.
    ///
    /// On the first call after `start_capture()` (initial capture or
    /// post-`resize`) the WGC pool can take several compositor ticks to begin
    /// emitting frames against the freshly-bound visual; observed in practice,
    /// a non-blocking poll right after `nudge_content` returns can race ahead
    /// and miss the first emission, leaving the consumer stuck on stale
    /// content. Block briefly here on the first attempt so the post-resize
    /// re-import reliably lands.
    pub fn try_acquire_frame(
        &mut self,
    ) -> Result<Option<WebView2CompositionFrame>, WryWebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }

        let needs_nudge = self
            .capture_state
            .as_ref()
            .map(|state| !state.first_frame_emitted)
            .unwrap_or(true);
        if needs_nudge {
            let _ = self.nudge_content(FIRST_FRAME_NUDGE_LABEL);
        }

        // Intentionally do NOT pump messages here in steady state. winit's
        // run-app loop is already pumping on this thread, and during a Win32
        // modal resize loop, peeking with `PM_REMOVE` from a render call
        // steals drag-tracking messages from the modal loop and causes
        // re-entrant `DispatchMessage` hangs. The first-frame block below
        // reinstates pumping for the post-`start_capture` warmup.

        let first_frame_deadline = if needs_nudge {
            Some(Instant::now() + Duration::from_millis(500))
        } else {
            None
        };

        let block_started = Instant::now();
        loop {
            let state = self
                .capture_state
                .as_mut()
                .expect("capture state populated above");
            match state.pool.TryGetNextFrame() {
                Ok(frame) => {
                    let captured = self.capture_frame_to_shared(frame)?;
                    return Ok(Some(captured));
                }
                Err(_) => match first_frame_deadline {
                    Some(deadline) if Instant::now() < deadline => {
                        // Pump messages so WebView2's composition commits
                        // propagate into the WGC pool.
                        pump_messages_for(Duration::from_millis(16));
                        continue;
                    }
                    Some(_) => {
                        eprintln!(
                            "[producer] first-frame block: TIMED OUT after {}ms",
                            block_started.elapsed().as_millis()
                        );
                        return Ok(None);
                    }
                    None => return Ok(None),
                },
            }
        }
    }

    /// Acquire the next capture frame with a caller-controlled timeout.
    pub fn acquire_frame_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<WebView2CompositionFrame, WryWebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }
        let needs_nudge = self
            .capture_state
            .as_ref()
            .map(|state| !state.first_frame_emitted)
            .unwrap_or(true);
        if needs_nudge {
            // Best-effort: a nudge failure should not abort the capture, since
            // WebView2 may still emit a frame on its own.
            let _ = self.nudge_content(FIRST_FRAME_NUDGE_LABEL);
        }

        let state = self
            .capture_state
            .as_mut()
            .expect("capture state populated above");

        let deadline = Instant::now() + timeout;
        let frame = loop {
            match state.pool.TryGetNextFrame() {
                Ok(frame) => break frame,
                Err(_) if Instant::now() < deadline => {
                    // Pump messages: WebView2's composition commits drive the
                    // WGC pool, and those commits propagate via Windows
                    // messages on this thread. (`start_capture` uses a plain
                    // sleep instead because dispatch-during-init has been
                    // observed to hang re-entrantly there.)
                    pump_messages_for(Duration::from_millis(16));
                }
                Err(error) => {
                    return Err(WryWebSurfaceError::Platform(format!(
                        "TryGetNextFrame timed out after {timeout:?} for {}x{}: {error}",
                        self.size.width, self.size.height
                    )));
                }
            }
        };

        self.capture_frame_to_shared(frame)
    }

    fn capture_frame_to_shared(
        &mut self,
        frame: windows::Graphics::Capture::Direct3D11CaptureFrame,
    ) -> Result<WebView2CompositionFrame, WryWebSurfaceError> {
        let content_size = frame
            .ContentSize()
            .map_err(platform("Direct3D11CaptureFrame::ContentSize"))?;
        let surface = frame
            .Surface()
            .map_err(platform("Direct3D11CaptureFrame::Surface"))?;
        let access = surface
            .cast::<IDirect3DDxgiInterfaceAccess>()
            .map_err(platform("IDirect3DSurface cast to IDirect3DDxgiInterfaceAccess"))?;
        let texture = unsafe { access.GetInterface::<ID3D11Texture2D>() }
            .map_err(platform("GetInterface<ID3D11Texture2D>"))?;
        let raw_texture = Interface::as_raw(&texture);

        self.generation = self.generation.saturating_add(1);
        let captured_size =
            PhysicalSize::new(content_size.Width as u32, content_size.Height as u32);

        let allocated_now = self.ensure_persistent_dest(captured_size)?;
        let dest = self
            .persistent_dest
            .as_mut()
            .expect("persistent_dest populated above");

        self.capture_factory
            .copy_capture_into_existing_target(
                &dest.texture.texture,
                WebView2D3D11CaptureFrame {
                    size: captured_size,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    generation: self.generation,
                    raw_d3d11_texture: raw_texture,
                },
            )?;

        let _ = frame.Close();

        if let Some(state) = self.capture_state.as_mut() {
            state.first_frame_emitted = true;
        }

        // The shared handle is only meaningful when the consumer has not yet
        // imported the current allocation. Hand it off exactly once, then null
        // it on every later frame so the consumer reliably reuses its
        // previously-imported `wgpu::Texture`.
        let resource_is_new = allocated_now || !dest.handle_handed_off;
        let shared_handle = if resource_is_new {
            dest.handle_handed_off = true;
            dest.texture.shared_frame.shared_handle
        } else {
            std::ptr::null_mut()
        };

        let surface_frame = WebView2DxgiSharedHandleFrame {
            size: captured_size,
            format: wgpu::TextureFormat::Bgra8Unorm,
            generation: self.generation,
            shared_handle,
        }
        .into_surface_frame();

        Ok(WebView2CompositionFrame {
            frame: surface_frame,
            content_size: captured_size,
            generation: self.generation,
            shared_handle,
            resource_is_new,
        })
    }

    fn ensure_persistent_dest(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<bool, WryWebSurfaceError> {
        if self
            .persistent_dest
            .as_ref()
            .map(|dest| dest.size == size)
            .unwrap_or(false)
        {
            return Ok(false);
        }

        // Re-allocating; drop the old D3D11 texture (the consumer's wgpu
        // texture, opened from the old NT handle, keeps that allocation alive
        // until the consumer drops it).
        self.persistent_dest = None;

        let texture = self.capture_factory.create_shared_texture(
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            self.generation,
        )?;
        self.persistent_dest = Some(PersistentDest {
            texture,
            size,
            handle_handed_off: false,
        });
        Ok(true)
    }

    fn start_capture(&mut self) -> Result<(), WryWebSurfaceError> {
        let started = Instant::now();
        if !GraphicsCaptureSession::IsSupported()
            .map_err(platform("GraphicsCaptureSession::IsSupported"))?
        {
            return Err(WryWebSurfaceError::Unsupported(
                "Windows.Graphics.Capture is not supported in this session",
            ));
        }

        // Give the WebView2 compositor time to commit *content* into the
        // visual before we bind a `GraphicsCaptureItem` to it. With a too-
        // short wait, the first WGC frame is the initial fully-transparent
        // buffer (BGRA all zeros) and any content-pixel validation fails.
        //
        // We deliberately do *not* pump Windows messages here: dispatching
        // mid-call has been observed to occasionally hang on a re-entrant
        // WebView2/WGC handler. Compositor commits run on a separate
        // thread, so a plain sleep is enough — we just need to wait long
        // enough for at least one WebView2 paint to land in the visual.
        std::thread::sleep(Duration::from_millis(500));

        let visual: Visual = self
            .webview_visual
            .cast()
            .map_err(platform("webview_visual cast to Visual"))?;
        let item = GraphicsCaptureItem::CreateFromVisual(&visual)
            .map_err(platform("GraphicsCaptureItem::CreateFromVisual"))?;
        let item_size = item.Size().map_err(platform("GraphicsCaptureItem::Size"))?;
        if item_size.Width <= 0 || item_size.Height <= 0 {
            return Err(WryWebSurfaceError::Platform(format!(
                "GraphicsCaptureItem returned invalid size {}x{}",
                item_size.Width, item_size.Height
            )));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &self.capture_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            item_size,
        )
        .map_err(platform("Direct3D11CaptureFramePool::CreateFreeThreaded"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(platform("CreateCaptureSession"))?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session
            .StartCapture()
            .map_err(platform("StartCapture"))?;

        self.capture_state = Some(CaptureState {
            item,
            pool,
            session,
            first_frame_emitted: false,
        });
        eprintln!(
            "[producer] start_capture: {}x{} ready in {}ms",
            item_size.Width,
            item_size.Height,
            started.elapsed().as_millis()
        );
        Ok(())
    }

    /// Inject a small JavaScript repaint hint after a capture-state change
    /// (e.g. just after `StartCapture`). Composition-controller WebView2s do
    /// not always issue a fresh paint until something invalidates layout.
    pub fn nudge_content(&self, label: &str) -> Result<(), WryWebSurfaceError> {
        let safe_label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
            .collect();
        let script = format!(
            r#"(() => new Promise(resolve => {{
                document.body.dataset.captureNudge = "{safe_label}";
                document.body.style.boxShadow = `inset 0 0 0 4px rgb(${{Math.floor(Math.random() * 255)}}, 190, 112)`;
                requestAnimationFrame(() => requestAnimationFrame(() => resolve("nudged")));
            }}))()"#
        );
        execute_script_blocking(&self.webview, script)
    }

    /// Direct access to the underlying `ICoreWebView2` for callers that need
    /// to attach event handlers, post Web messages, or invoke JS.
    pub fn webview(&self) -> &ICoreWebView2 {
        &self.webview
    }

    /// Direct access to the underlying `ICoreWebView2Controller`.
    pub fn controller(&self) -> &ICoreWebView2Controller {
        &self.controller
    }
}

impl Drop for WebView2CompositionProducer {
    fn drop(&mut self) {
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
            let _ = state;
        }
        unsafe {
            let _ = self.controller.Close();
        }
    }
}

impl crate::WryWebSurfaceProducer for WebView2CompositionProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        // Windows can produce a `Dx12SharedTexture` whenever the host's
        // wgpu device is on the DX12 backend; the host context isn't
        // visible from inside the producer, so we report the shape we
        // actually emit (`Dx12SharedTexture` frames) and leave the
        // host-backend match-up to the consumer's import call.
        WryWebSurfaceCapabilities {
            backend: SystemWebviewBackend::WebView2,
            preferred_mode: WebSurfaceMode::ImportedTexture,
            imported_texture: wgpu_native_texture_interop::CapabilityStatus::Supported,
            native_child_overlay:
                wgpu_native_texture_interop::CapabilityStatus::Supported,
            cpu_snapshot: wgpu_native_texture_interop::CapabilityStatus::Supported,
            supported_frames: vec![
                wgpu_native_texture_interop::NativeFrameKind::Dx12SharedTexture,
            ],
            reason: "WebView2 CompositionController visual + Windows.Graphics.Capture + shared D3D11 NT-handle texture imported as Dx12SharedTexture.",
        }
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        let full = self.acquire_full_frame()?;
        Ok(full.frame)
    }

    fn navigate_to_string(
        &mut self,
        html: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        WebView2CompositionProducer::navigate_to_string(self, html, timeout)
    }

    fn resize(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<(), WryWebSurfaceError> {
        WebView2CompositionProducer::resize(self, size)
    }

    fn set_offset(&mut self, x: f32, y: f32) -> Result<(), WryWebSurfaceError> {
        WebView2CompositionProducer::set_offset(self, x, y)
    }
}

fn create_environment(user_data_dir: &Path) -> Result<ICoreWebView2Environment, WryWebSurfaceError> {
    if let Err(error) = std::fs::create_dir_all(user_data_dir) {
        return Err(WryWebSurfaceError::Platform(format!(
            "create user_data_dir {}: {error}",
            user_data_dir.display()
        )));
    }
    let user_data_dir = user_data_dir.to_string_lossy().into_owned();

    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let user_data_dir = CoTaskMemPWSTR::from(user_data_dir.as_str());
            let options = CoreWebView2EnvironmentOptions::default();
            unsafe {
                webview2_com::Microsoft::Web::WebView2::Win32::CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    *user_data_dir.as_ref().as_pcwstr(),
                    &ICoreWebView2EnvironmentOptions::from(options),
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(move |error_code, environment| {
            error_code?;
            tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send over mpsc channel");
            Ok(())
        }),
    )
    .map_err(|error| WryWebSurfaceError::Platform(format!("CreateCoreWebView2Environment: {error}")))?;

    rx.recv()
        .map_err(|_| {
            WryWebSurfaceError::Platform(
                "CreateCoreWebView2Environment completion channel closed".to_string(),
            )
        })?
        .map_err(platform("CreateCoreWebView2Environment result"))
}

fn create_composition_controller(
    environment: &ICoreWebView2Environment,
    parent_hwnd: HWND,
) -> Result<ICoreWebView2CompositionController, WryWebSurfaceError> {
    let environment3: ICoreWebView2Environment3 = environment
        .cast()
        .map_err(platform("environment cast to ICoreWebView2Environment3"))?;
    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            environment3
                .CreateCoreWebView2CompositionController(parent_hwnd, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error_code, controller| {
            error_code?;
            tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send over mpsc channel");
            Ok(())
        }),
    )
    .map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "CreateCoreWebView2CompositionController: {error}"
        ))
    })?;

    rx.recv()
        .map_err(|_| {
            WryWebSurfaceError::Platform(
                "CreateCoreWebView2CompositionController completion channel closed".to_string(),
            )
        })?
        .map_err(platform("CreateCoreWebView2CompositionController result"))
}

fn execute_script_blocking(
    webview: &ICoreWebView2,
    script: String,
) -> Result<(), WryWebSurfaceError> {
    let webview = webview.clone();
    ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            let script = CoTaskMemPWSTR::from(script.as_str());
            webview
                .ExecuteScript(*script.as_ref().as_pcwstr(), &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(|error_code, _result| error_code),
    )
    .map_err(|error| WryWebSurfaceError::Platform(format!("ExecuteScript: {error}")))
}

fn pump_messages_for(duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn pump_until(timeout: Duration, rx: &mpsc::Receiver<()>) -> Result<(), ()> {
    let deadline = Instant::now() + timeout;
    loop {
        if rx.try_recv().is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(());
        }
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                if rx.try_recv().is_ok() {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn platform<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> WryWebSurfaceError {
    move |error| WryWebSurfaceError::Platform(format!("{context} failed: {error}"))
}
