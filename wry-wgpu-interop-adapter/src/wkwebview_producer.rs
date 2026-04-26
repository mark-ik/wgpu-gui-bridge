//! macOS WKWebView capture producer (planning skeleton).
//!
//! This is the macOS counterpart to
//! [`crate::webview2_composition_producer::WebView2CompositionProducer`].
//! The shape mirrors the Windows producer so consumers can program
//! against a single trait surface (`WryWebSurfaceProducer`); the
//! *internals* are entirely different because macOS has no public
//! composition-capture API directly analogous to
//! `Windows.Graphics.Capture::CreateFromVisual`.
//!
//! ## Capture options on macOS
//!
//! 1. **`WKWebView.takeSnapshot(...)` → CPU pixels.**
//!    Public, simple, returns an `NSImage`. One-shot per call: each
//!    invocation schedules a fresh render pass. Latency is high
//!    (typically >50ms) and rate is well below display refresh, so this
//!    is a `CpuSnapshot`-tier capability — useful for thumbnails and
//!    offscreen layout inspection, not for an interactive composited
//!    surface.
//!
//! 2. **`ScreenCaptureKit` (macOS 12.3+) → `IOSurfaceRef` →
//!    `MTLTexture`.** The closest analog to `Windows.Graphics.Capture`.
//!    Bind an `SCContentFilter` to either the `NSWindow` hosting the
//!    `WKWebView` or directly to the WKWebView's underlying `CALayer`,
//!    configure an `SCStreamConfiguration` for `BGRA8Unorm`, and stream
//!    frames via `SCStreamOutput`. Each `CMSampleBuffer` carries a
//!    `CVPixelBuffer` whose backing `IOSurfaceRef` maps to a Metal
//!    texture via `MTLDevice::newTextureWithDescriptor:iosurface:plane:`.
//!    This is the intended `ImportedTexture` path. Requires the
//!    "Screen Recording" privacy permission to be granted to the host
//!    binary on first use.
//!
//! 3. **Direct `CALayer` contents observation (private SPI).** WKWebView
//!    is layer-backed; the web-content compositing layer ultimately
//!    holds an `IOSurface`. Reaching it requires SPI / undocumented
//!    interfaces (`-_swapChain`, `-WKLayerHostView`, etc.), is fragile
//!    across macOS versions, and would not be acceptable in App Store
//!    builds. Worth knowing as an emergency hatch but not the canonical
//!    path.
//!
//! ## Sync model
//!
//! Friendlier than the D3D11/D3D12 case on Windows:
//!
//! - `IOSurface` is shared memory with cache-coherence guarantees the
//!   OS manages, *as long as* the producer/consumer pattern is honored
//!   (one writer, multiple readers). ScreenCaptureKit is the writer.
//! - For explicit GPU↔GPU sync, `MTLSharedEvent` is the Metal analog of
//!   a D3D12 fence. ScreenCaptureKit owns its own GPU queue; the
//!   consumer (wgpu's Metal queue) needs a wait point only if
//!   cross-queue ordering matters. For "render the most recent frame
//!   each present" semantics, implicit IOSurface coherence is enough.
//! - The Windows producer's "transition-barrier cache flush" trick
//!   has no Metal analog — IOSurface-backed Metal textures are
//!   `Storage::Shared` and don't need a per-frame barrier.
//!
//! ## Producer lifecycle
//!
//! Mirrors `WebView2CompositionProducer`:
//!
//! - `new(parent_view, config)` builds a `WKWebView` configured for
//!   composition capture and adds it as a subview of `parent_view`.
//! - `navigate_to_string(html, timeout)` loads inline HTML and waits
//!   for `WKNavigationDelegate.didFinishNavigation:`.
//! - `start_capture()` (lazy, triggered by first acquire) constructs
//!   an `SCContentFilter` over the WebView's window, builds an
//!   `SCStream` + `SCStreamConfiguration`, and calls `startCaptureWithCompletionHandler:`.
//! - `try_acquire_frame()` pulls the most recent `CMSampleBuffer`'s
//!   `IOSurfaceRef`, wraps it as `MTLTexture`, and returns it via
//!   `WryWebSurfaceFrame::Native(NativeFrame::Metal(MetalTextureRef))`.
//! - `resize(size)` updates the WKWebView's `frame.size`, the
//!   `SCStreamConfiguration.width/height`, and reapplies the filter.
//! - `Drop` stops the stream and tears down the WKWebView.
//!
//! ## Imports the implementer will need
//!
//! When filling this in on a Mac, these are the crate paths the real
//! implementation reaches for. Listed here so the next session doesn't
//! have to chase them down across docs.rs:
//!
//! ```text
//! objc2::rc::Retained
//! objc2::runtime::ProtocolObject
//! objc2_foundation::{NSString, NSURL, NSDate, NSError}
//! objc2_foundation::{CGRect, CGPoint, CGSize}
//! objc2_app_kit::NSView
//! objc2_web_kit::{WKWebView, WKWebViewConfiguration, WKNavigationDelegate}
//! objc2_screen_capture_kit::{SCContentFilter, SCStream, SCStreamConfiguration,
//!     SCShareableContent, SCStreamOutput, SCStreamOutputType}
//! objc2_core_media::CMSampleBuffer
//! objc2_core_video::{CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey}
//! objc2_io_surface::IOSurfaceRef
//! objc2_metal::{MTLDevice, MTLTexture, MTLTextureDescriptor}
//! block2::{Block, RcBlock, StackBlock}
//! dispatch2::Queue
//! ```
//!
//! ## Status
//!
//! This module is a **planning skeleton with macOS deps locked in**.
//! `cargo build -p wry-wgpu-interop-adapter` on macOS 12.3+ should
//! succeed: the type satisfies `WryWebSurfaceProducer` via the
//! crate-level default impls (`Unsupported` for navigate/resize/offset,
//! `OverlayOnly` from `acquire_frame`). The actual WKWebView host,
//! ScreenCaptureKit binding, and IOSurface→Metal handoff are the next
//! milestones for the macOS producer slice.

#![cfg(target_os = "macos")]

use std::path::PathBuf;

use dpi::PhysicalSize;

use crate::{
    SystemWebviewBackend, WebSurfaceMode, WryWebSurfaceCapabilities, WryWebSurfaceError,
    WryWebSurfaceFrame, WryWebSurfaceProducer,
};

/// Configuration for `WkWebViewProducer::new`. Mirrors the shape of
/// [`crate::webview2_composition_producer::WebView2CompositionConfig`].
#[derive(Clone, Debug)]
pub struct WkWebViewProducerConfig {
    /// Initial size of the WKWebView frame and the capture region, in
    /// physical pixels.
    pub size: PhysicalSize<u32>,
    /// Offset of the WKWebView relative to the parent NSView, in
    /// device-independent points (matches AppKit's coordinate system).
    pub offset: (f32, f32),
    /// Directory used as `WKWebsiteDataStore`'s persistent storage.
    pub data_dir: PathBuf,
    /// Timeout for `navigate_to_string`, mirroring the Windows
    /// producer's navigation completion wait.
    pub navigation_timeout: std::time::Duration,
    /// Timeout for the initial frame after `start_capture`. Mirrors the
    /// Windows producer's first-frame block.
    pub frame_timeout: std::time::Duration,
}

impl WkWebViewProducerConfig {
    pub fn new(size: PhysicalSize<u32>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            size,
            offset: (0.0, 0.0),
            data_dir: data_dir.into(),
            navigation_timeout: std::time::Duration::from_secs(5),
            frame_timeout: std::time::Duration::from_secs(2),
        }
    }

    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset = (x, y);
        self
    }
}

/// Skeleton WKWebView capture producer.
///
/// See the module-level docs for the intended capture path
/// (ScreenCaptureKit → IOSurface → Metal). The current implementation
/// is a stub that satisfies `WryWebSurfaceProducer` via the trait's
/// default impls.
pub struct WkWebViewProducer {
    capabilities: WryWebSurfaceCapabilities,
    // Real producer state (WKWebView, SCStream, current CMSampleBuffer,
    // shared IOSurface-backed MTLTexture, etc.) lives here once the
    // macOS implementer fills it in. Kept off the struct in this
    // skeleton so the module compiles regardless of objc2 feature
    // selection wobbles.
}

impl WkWebViewProducer {
    /// Construct the producer skeleton.
    ///
    /// **Implementation outline** (for the Mac iteration session):
    ///
    /// 1. Cast `parent_view` to `Retained<NSView>` via
    ///    `Retained::retain(NonNull::new_unchecked(parent_view as *mut NSView))`.
    /// 2. Build `WKWebViewConfiguration` (`WKWebsiteDataStore` pointing
    ///    at `config.data_dir`, default `WKPreferences`).
    /// 3. Create `WKWebView` with `initWithFrame:configuration:` using
    ///    `CGRect { origin: config.offset, size: config.size }` (in
    ///    points; convert physical → points using
    ///    `parent_view.window().backingScaleFactor()`).
    /// 4. `parent_view.addSubview(&webview)`.
    /// 5. Initialize a `WKNavigationDelegate` impl that pushes
    ///    `didFinishNavigation:` events through an mpsc channel for
    ///    `navigate_to_string` to wait on.
    /// 6. Return `Self { capabilities: ScreenCaptureKit-aware, ... }`.
    ///
    /// # Safety
    ///
    /// `parent_view` must be a valid `NSView *` that outlives the
    /// producer.
    pub unsafe fn new(
        _parent_view: *mut std::ffi::c_void,
        _config: WkWebViewProducerConfig,
    ) -> Result<Self, WryWebSurfaceError> {
        Ok(Self {
            capabilities: WryWebSurfaceCapabilities {
                backend: SystemWebviewBackend::WkWebView,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: wgpu_native_texture_interop::CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: wgpu_native_texture_interop::CapabilityStatus::Supported,
                cpu_snapshot: wgpu_native_texture_interop::CapabilityStatus::Supported,
                supported_frames: Vec::new(),
                reason: "WkWebViewProducer is a planning skeleton; ScreenCaptureKit + IOSurface → Metal capture is not yet wired.",
            },
        })
    }

    /// Acquire a content-pixel `WKWebView.takeSnapshot` for diagnostics
    /// (the `CpuSnapshot`-tier path). Returns a CPU RGBA frame.
    ///
    /// **Implementation outline:**
    ///
    /// 1. `let cfg = WKSnapshotConfiguration::new()` (default: full bounds).
    /// 2. `webview.takeSnapshotWithConfiguration:completionHandler:`
    ///    with a block that signals an mpsc on completion.
    /// 3. Block on the channel up to a timeout, pumping the run-loop
    ///    via `RunLoop::current().runMode:beforeDate:` (the macOS
    ///    analog of `pump_messages_for`).
    /// 4. Convert the resulting `NSImage` → `CGImage` →
    ///    `vImageBuffer`/`CGContext` → `image::RgbaImage` and return it
    ///    as `WryWebSurfaceFrame::CpuRgba { ... }`.
    pub fn capture_cpu_snapshot(
        &mut self,
    ) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WkWebViewProducer::capture_cpu_snapshot is not implemented yet",
        ))
    }

    /// Non-blocking acquire. Once implemented this returns
    /// `Some(WryWebSurfaceFrame::Native(NativeFrame::Metal(...)))`
    /// when the most recent `CMSampleBuffer` has landed; otherwise
    /// `None`.
    ///
    /// **Implementation outline:**
    ///
    /// 1. The `SCStreamOutput` delegate set up by `start_capture` keeps
    ///    a `Mutex<Option<Retained<CMSampleBuffer>>>` of the latest
    ///    sample buffer (replacing on each callback — we only render
    ///    the most recent).
    /// 2. `try_acquire_frame` `take()`s the latest. If `None`, return
    ///    `Ok(None)`.
    /// 3. Extract `CVPixelBuffer` from the `CMSampleBuffer` via
    ///    `CMSampleBufferGetImageBuffer`.
    /// 4. Extract `IOSurfaceRef` via `CVPixelBufferGetIOSurface`.
    /// 5. Wrap as `MTLTexture` with
    ///    `MTLDevice::newTextureWithDescriptor:iosurface:plane:` (plane
    ///    0 for BGRA). The descriptor uses
    ///    `pixelFormat = .bgra8Unorm`, `usage = [.shaderRead]`,
    ///    `storageMode = .shared`.
    /// 6. Build a `wgpu_native_texture_interop::MetalTextureRef`
    ///    pointing at the raw `MTLTexture *`, return as
    ///    `WryWebSurfaceFrame::Native(NativeFrame::Metal(...))`.
    pub fn try_acquire_frame(
        &mut self,
    ) -> Result<Option<WryWebSurfaceFrame>, WryWebSurfaceError> {
        Ok(None)
    }
}

impl WryWebSurfaceProducer for WkWebViewProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        // Until ScreenCaptureKit is wired in, the WKWebView remains a
        // platform overlay child and the consumer composites it
        // itself.
        Ok(WryWebSurfaceFrame::OverlayOnly)
    }

    /// Navigate to inline HTML.
    ///
    /// **Implementation outline:**
    ///
    /// 1. `webview.loadHTMLString:&NSString::from_str(html), baseURL:None`.
    /// 2. Wait on the navigation-completion mpsc set up in `new`,
    ///    pumping the run-loop until either the channel signals or
    ///    `timeout` elapses.
    /// 3. Return `Ok(())` on completion, `Err(Platform)` on timeout.
    fn navigate_to_string(
        &mut self,
        _html: &str,
        _timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WkWebViewProducer::navigate_to_string is not implemented yet",
        ))
    }

    /// Resize the WebView and the SC stream.
    ///
    /// **Implementation outline:**
    ///
    /// 1. Convert physical pixels to AppKit points via the parent
    ///    window's `backingScaleFactor`.
    /// 2. `webview.setFrameSize(NSSize { width, height })`.
    /// 3. If a stream is active, update
    ///    `SCStreamConfiguration.width/height` and call
    ///    `stream.updateConfiguration:` (or restart the stream — Apple
    ///    docs say `updateConfiguration:` is preferred but stream
    ///    restart is the fallback if update fails on the current OS
    ///    version).
    fn resize(&mut self, _size: PhysicalSize<u32>) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WkWebViewProducer::resize is not implemented yet",
        ))
    }

    /// Reposition the WebView within the parent view.
    ///
    /// **Implementation outline:**
    /// `webview.setFrameOrigin(NSPoint { x, y })`. Coordinates are in
    /// AppKit points, not physical pixels — divide by
    /// `backingScaleFactor` if the caller hands us pixels.
    fn set_offset(&mut self, _x: f32, _y: f32) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WkWebViewProducer::set_offset is not implemented yet",
        ))
    }
}
