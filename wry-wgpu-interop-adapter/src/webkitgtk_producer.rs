//! Linux WebKitGTK / WPE capture producer (planning skeleton).
//!
//! This is the Linux counterpart to
//! [`crate::webview2_composition_producer::WebView2CompositionProducer`].
//! Of the three platforms this is the most speculative: Linux has no
//! ergonomic public API for capturing a `WebKitGTK` widget's compositor
//! output as a GPU surface today.
//!
//! ## System packages required
//!
//! On a fresh Linux dev box (Debian / Ubuntu — translate as needed for
//! Fedora / Arch):
//!
//! ```sh
//! sudo apt install -y \
//!     libwebkit2gtk-4.1-dev \
//!     libgtk-3-dev \
//!     pkg-config \
//!     build-essential
//! ```
//!
//! `webkit2gtk-4.1` is the line shipped on Ubuntu 22.04+ / Debian 12+;
//! older distros may have only `webkit2gtk-4.0` (matching the older
//! `webkit2gtk = "0.18"` Rust crate).
//!
//! ## Capture options on Linux
//!
//! 1. **`webkit_web_view_get_snapshot(...)` → CPU pixels.**
//!    Public, returns a `cairo::ImageSurface`. The same trade-off as
//!    `WKWebView.takeSnapshot` on macOS: viable as a `CpuSnapshot`-tier
//!    fallback for previews and thumbnails, not a path to interactive
//!    composited frames. This is what
//!    [`WebKitGtkProducer::capture_cpu_snapshot`] will hook up.
//!
//! 2. **WPE WebKit + `WPEViewBackendDMABuf` → DMABUF → Vulkan external
//!    memory → wgpu Vulkan.** `WebKit2GTK` and `WPEWebKit` share a
//!    backend; `WPEViewBackendDMABuf` publishes the page's compositor
//!    output as a sequence of DMABUF file descriptors. On the consumer
//!    side, Vulkan's `VK_KHR_external_memory_fd` +
//!    `VK_EXT_image_drm_format_modifier` can import them as `VkImage`s,
//!    and wgpu's Vulkan backend can wrap the `VkImage` via wgpu-hal's
//!    `Device::texture_from_raw`. **This is the intended
//!    `ImportedTexture` path** — directly analogous to what we do on
//!    Windows with NT-handle shared D3D textures, but using DMABUF +
//!    `VkSemaphore` instead of NT handles + keyed mutex.
//!
//!    Cost: meaningful. The DMABUF backend is still considered an
//!    extension API by upstream WebKit and the public Rust bindings
//!    (`webkit2gtk`, `wpe`) do not expose it. Likely a custom GObject
//!    interop shim using `glib-sys` and `wpe-sys`.
//!
//! 3. **Wayland-side capture (wlroots' `zwlr_screencopy_manager_v1`).**
//!    Capture the whole top-level surface from the compositor instead
//!    of the WebKitGTK widget specifically. Works on wlroots-based
//!    compositors (Sway, Hyprland, river); not on GNOME Mutter, KDE
//!    KWin, or XFCE (X11). Equivalent to using `Windows.Graphics.Capture`
//!    against the host HWND rather than the WebView2 visual — coarser,
//!    requires layout coordination so the WebView region is captured
//!    cleanly. Could be the pragmatic first `ImportedTexture` path
//!    while the WPE DMABUF route gets sorted, but won't help on the
//!    common XFCE / GNOME desktops.
//!
//! ## Sync model (sketch)
//!
//! For the DMABUF path:
//!
//! - The producer is the WebKit page-composition process. It exports
//!   buffers via DMABUF file descriptors plus `VkSemaphore`
//!   opaque-fd handles for sync (or, on older kernels, `dma-fence`
//!   exposed through `EGL_ANDROID_native_fence_sync`).
//! - The consumer (wgpu Vulkan) imports the `VkImage` via
//!   `VkImportMemoryFdInfoKHR`, imports the semaphore via
//!   `VkImportSemaphoreFdInfoKHR`, and the per-frame protocol is
//!   `vkQueueSubmit(... waitSemaphores: [acquired_sem] ...)`.
//! - This is structurally the same as the D3D12 fence path described
//!   in the adapter README's option-3 future-work note: a shared GPU
//!   fence/semaphore makes the cross-API ordering explicit. There is
//!   no `IOSurface`-style "OS handles cache coherence" fallback on
//!   Linux/Vulkan — explicit sync via semaphores is the only path.
//!
//! ## Producer lifecycle
//!
//! Identical shape to the Windows / macOS producers:
//!
//! - `new(parent_widget, config)` builds a `WebKitWebView` and packs
//!   it into the parent `GtkContainer`.
//! - `navigate_to_string(html, timeout)` for inline HTML.
//! - `start_capture()` brings up the chosen capture path (DMABUF or
//!   wlroots screencopy).
//! - `try_acquire_frame()` returns the next imported frame, or `None`.
//! - `resize(size)` adjusts the `GtkWidget` allocation and capture
//!   buffer size.
//!
//! ## Imports the implementer will need
//!
//! ```text
//! gtk::prelude::{ContainerExt, WidgetExt}
//! gtk::{Container, Widget}
//! webkit2gtk::{WebView, WebViewBuilder, WebViewExt, WebContext, Settings}
//! webkit2gtk::{NavigationPolicyDecision, LoadEvent}
//! glib::{MainContext, MainLoop, Continue}
//! cairo::{ImageSurface, Format}                           // for CPU snapshot
//! ash::vk                                                 // for VK_KHR_external_memory_fd
//! ```
//!
//! ## Status
//!
//! This module is a **planning skeleton with Linux deps locked in**.
//! `cargo build -p wry-wgpu-interop-adapter` on a Linux box with the
//! apt packages installed should succeed: the type satisfies
//! `WryWebSurfaceProducer` via the crate-level default impls
//! (`Unsupported` for navigate/resize/offset, `OverlayOnly` from
//! `acquire_frame`). The actual WebKitWebView host, DMABUF capture
//! backend, and Vulkan external-memory handoff are the next milestones
//! for the Linux producer slice.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use dpi::PhysicalSize;

use crate::{
    SystemWebviewBackend, WebSurfaceMode, WryWebSurfaceCapabilities, WryWebSurfaceError,
    WryWebSurfaceFrame, WryWebSurfaceProducer,
};

/// Configuration for `WebKitGtkProducer::new`. Mirrors the Windows /
/// macOS configs.
#[derive(Clone, Debug)]
pub struct WebKitGtkProducerConfig {
    /// Initial size of the `WebKitWebView` allocation and the capture
    /// region, in physical pixels.
    pub size: PhysicalSize<u32>,
    /// Offset of the `WebKitWebView` relative to the parent container,
    /// in device-independent pixels.
    pub offset: (f32, f32),
    /// Directory used as the `WebKitWebContext`'s data directory.
    pub data_dir: PathBuf,
    /// Timeout for `navigate_to_string`, mirroring the Windows
    /// producer's navigation completion wait.
    pub navigation_timeout: std::time::Duration,
    /// Timeout for the initial frame after `start_capture`. Mirrors
    /// the Windows producer's first-frame block.
    pub frame_timeout: std::time::Duration,
}

impl WebKitGtkProducerConfig {
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

/// Skeleton WebKitGTK / WPE capture producer.
///
/// See the module-level docs for the intended capture path (WPE
/// DMABUF + Vulkan external memory, or wlroots screencopy as a
/// compositor-restricted fallback). The current implementation is a
/// stub that satisfies `WryWebSurfaceProducer` via the trait's
/// default impls.
pub struct WebKitGtkProducer {
    capabilities: WryWebSurfaceCapabilities,
    // Real producer state (`WebKitWebView`, GTK container, DMABUF
    // import context, current `VkImage` + semaphore, etc.) lives here
    // once the Linux implementer fills it in.
}

impl WebKitGtkProducer {
    /// Construct the producer skeleton.
    ///
    /// **Implementation outline** (for the Linux iteration session):
    ///
    /// 1. Cast `parent_widget` to `gtk::Widget` via the GObject FFI
    ///    cast helpers (`from_glib_none(parent_widget as *mut _)`).
    /// 2. Create a `webkit2gtk::WebContext` rooted at `config.data_dir`
    ///    via `WebContext::new_ephemeral` or
    ///    `WebContextBuilder::new().website_data_manager(...).build()`.
    /// 3. Build the `WebView` via
    ///    `WebViewBuilder::new().web_context(&ctx).build()`. Configure
    ///    `WebViewSettings` for hardware acceleration if needed.
    /// 4. Add the `WebView` to the parent container with
    ///    `container.add(&webview); webview.set_size_request(w, h);`.
    /// 5. Connect the `load_changed` signal to push `LoadEvent::Finished`
    ///    events through an mpsc channel for `navigate_to_string` to
    ///    wait on.
    /// 6. Return `Self { capabilities: DMABUF-aware once implemented, ... }`.
    ///
    /// # Safety
    ///
    /// `parent_widget` must be a valid `GtkWidget *` (any GTK 3
    /// container) that outlives the producer.
    pub unsafe fn new(
        _parent_widget: *mut std::ffi::c_void,
        _config: WebKitGtkProducerConfig,
    ) -> Result<Self, WryWebSurfaceError> {
        Ok(Self {
            capabilities: WryWebSurfaceCapabilities {
                backend: SystemWebviewBackend::WebKitGtk,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: wgpu_native_texture_interop::CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: wgpu_native_texture_interop::CapabilityStatus::Supported,
                cpu_snapshot: wgpu_native_texture_interop::CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                supported_frames: Vec::new(),
                reason: "WebKitGtkProducer is a planning skeleton; the WPE DMABUF / wlroots screencopy paths are not yet wired.",
            },
        })
    }

    /// Acquire a `webkit_web_view_get_snapshot` for diagnostics (the
    /// `CpuSnapshot`-tier path). Returns a CPU RGBA frame.
    ///
    /// **Implementation outline:**
    ///
    /// 1. `webview.snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, gio::Cancellable::NONE, |result| { ... })`.
    /// 2. The completion closure delivers a `cairo::ImageSurface`. Push
    ///    it through an mpsc.
    /// 3. Block on the mpsc, pumping the GTK main context with
    ///    `glib::MainContext::default().iteration(false)` (the Linux
    ///    analog of `pump_messages_for`).
    /// 4. `surface.with_data(...)` to get the BGRA32 pixels, swizzle
    ///    to RGBA, return as
    ///    `WryWebSurfaceFrame::CpuRgba { size, pixels: image::RgbaImage::from_raw(...), generation }`.
    pub fn capture_cpu_snapshot(
        &mut self,
    ) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WebKitGtkProducer::capture_cpu_snapshot is not implemented yet",
        ))
    }

    /// Non-blocking acquire. Once implemented this returns
    /// `Some(WryWebSurfaceFrame::Native(NativeFrame::VulkanExternalImage(...)))`
    /// when the next DMABUF frame has landed; otherwise `None`.
    ///
    /// **Implementation outline (DMABUF path):**
    ///
    /// 1. The WPE backend exposes a buffer-export callback. Each
    ///    callback delivers `(dma_buf_fd, drm_format_modifier,
    ///    vk_semaphore_fd, width, height)`. Stash the latest in a
    ///    `Mutex<Option<...>>` (drop older buffers — we only render
    ///    the most recent).
    /// 2. `try_acquire_frame` `take()`s the latest. If `None`, return
    ///    `Ok(None)`.
    /// 3. Build a `VkImage` via `VkImportMemoryFdInfoKHR` +
    ///    `VkExternalMemoryImageCreateInfo` +
    ///    `VkImageDrmFormatModifierExplicitCreateInfoEXT`.
    /// 4. Wrap as a wgpu texture via wgpu-hal's
    ///    `vulkan::Device::texture_from_raw`.
    /// 5. Build a `wgpu_native_texture_interop::VulkanExternalImage`
    ///    pointing at the wgpu texture, return as
    ///    `WryWebSurfaceFrame::Native(NativeFrame::Vulkan(...))`.
    /// 6. The consumer's render must `vkQueueSubmit` with
    ///    `waitSemaphores = [imported_acquire_sem]` so the GPU waits
    ///    for the producer's writes before sampling.
    pub fn try_acquire_frame(
        &mut self,
    ) -> Result<Option<WryWebSurfaceFrame>, WryWebSurfaceError> {
        Ok(None)
    }
}

impl WryWebSurfaceProducer for WebKitGtkProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        Ok(WryWebSurfaceFrame::OverlayOnly)
    }

    /// Navigate to inline HTML.
    ///
    /// **Implementation outline:**
    ///
    /// 1. `webview.load_html(html, None)`.
    /// 2. Wait on the `load_changed` mpsc set up in `new`, pumping the
    ///    GTK main context until either `LoadEvent::Finished` arrives
    ///    or `timeout` elapses.
    /// 3. Return `Ok(())` on completion, `Err(Platform)` on timeout.
    fn navigate_to_string(
        &mut self,
        _html: &str,
        _timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WebKitGtkProducer::navigate_to_string is not implemented yet",
        ))
    }

    /// Resize the WebView and the capture pipeline.
    ///
    /// **Implementation outline:**
    ///
    /// 1. `webview.set_size_request(width as i32, height as i32)`.
    /// 2. If a capture stream is active, reconfigure the WPE
    ///    `WPEViewBackendDMABuf` output dimensions (typically by
    ///    re-creating the backend, since the DMABUF protocol's
    ///    resolution is set at construction time). Or, if on the
    ///    wlroots fallback, update the screencopy frame request size.
    fn resize(&mut self, _size: PhysicalSize<u32>) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WebKitGtkProducer::resize is not implemented yet",
        ))
    }

    /// Reposition the WebView within the parent container.
    ///
    /// **Implementation outline:**
    /// For a `gtk::Fixed` parent, `parent.move_(&webview, x as i32, y as i32)`.
    /// For a `gtk::Box` or `gtk::Grid`, the offset is determined by
    /// pack/attach order rather than free positioning, so this is a
    /// no-op for those layouts. Match the parent layout choice in
    /// `new`.
    fn set_offset(&mut self, _x: f32, _y: f32) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WebKitGtkProducer::set_offset is not implemented yet",
        ))
    }
}
