#![doc = include_str!("../README.md")]

use dpi::PhysicalSize;
use thiserror::Error;
use wgpu_native_texture_interop::{
    CapabilityStatus, HostWgpuContext, InteropBackend, InteropError, NativeFrame, NativeFrameKind,
    ProducerCapabilities,
};

#[cfg(target_os = "windows")]
pub mod windows_capture;

#[cfg(target_os = "windows")]
pub mod webview2_composition_producer;

#[cfg(target_os = "macos")]
pub mod wkwebview_producer;

#[cfg(target_os = "linux")]
pub mod webkitgtk_producer;

/// How a system webview can participate in a host compositor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WebSurfaceMode {
    /// The adapter can emit a native GPU frame importable by `wgpu-native-texture-interop`.
    ImportedTexture,
    /// The webview must remain a platform child window/visual overlay.
    NativeChildOverlay,
    /// The adapter can emit CPU pixels or encoded snapshots.
    CpuSnapshot,
    /// No usable surface path is available.
    Unsupported,
}

/// The system webview backend behind Wry on the current platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SystemWebviewBackend {
    WebView2,
    WkWebView,
    WebKitGtk,
    Unknown,
}

impl SystemWebviewBackend {
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            Self::WebView2
        } else if cfg!(target_os = "macos") {
            Self::WkWebView
        } else if cfg!(target_os = "linux") {
            Self::WebKitGtk
        } else {
            Self::Unknown
        }
    }
}

/// Probe result for a Wry/system-webview surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WryWebSurfaceCapabilities {
    pub backend: SystemWebviewBackend,
    pub preferred_mode: WebSurfaceMode,
    pub imported_texture: CapabilityStatus,
    pub native_child_overlay: CapabilityStatus,
    pub cpu_snapshot: CapabilityStatus,
    pub supported_frames: Vec<NativeFrameKind>,
    pub reason: &'static str,
}

impl WryWebSurfaceCapabilities {
    pub fn probe(host: Option<&HostWgpuContext>) -> Self {
        match SystemWebviewBackend::detect() {
            SystemWebviewBackend::WebView2 => probe_webview2(host),
            SystemWebviewBackend::WkWebView => Self {
                backend: SystemWebviewBackend::WkWebView,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: CapabilityStatus::Supported,
                cpu_snapshot: CapabilityStatus::Supported,
                supported_frames: Vec::new(),
                reason: "WKWebView snapshot capture is useful as a fallback, but no Metal texture producer is wired.",
            },
            SystemWebviewBackend::WebKitGtk => Self {
                backend: SystemWebviewBackend::WebKitGtk,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: CapabilityStatus::Supported,
                cpu_snapshot: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                supported_frames: Vec::new(),
                reason: "WebKitGTK has internal DMABUF presentation paths, but Wry does not expose them as a frame producer.",
            },
            SystemWebviewBackend::Unknown => Self {
                backend: SystemWebviewBackend::Unknown,
                preferred_mode: WebSurfaceMode::Unsupported,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                native_child_overlay: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                cpu_snapshot: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                supported_frames: Vec::new(),
                reason: "No Wry/system-webview backend is defined for this platform.",
            },
        }
    }

    pub fn producer_capabilities(&self) -> ProducerCapabilities {
        ProducerCapabilities {
            supported_frames: self.supported_frames.clone(),
        }
    }
}

fn probe_webview2(host: Option<&HostWgpuContext>) -> WryWebSurfaceCapabilities {
    let imported_texture = match host.map(|host| host.backend) {
        Some(InteropBackend::Dx12) => CapabilityStatus::Supported,
        Some(_) => CapabilityStatus::Unsupported(
            wgpu_native_texture_interop::UnsupportedReason::HostBackendMismatch,
        ),
        None => CapabilityStatus::Unsupported(
            wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
        ),
    };

    let preferred_mode = if imported_texture == CapabilityStatus::Supported {
        WebSurfaceMode::ImportedTexture
    } else {
        WebSurfaceMode::NativeChildOverlay
    };

    WryWebSurfaceCapabilities {
        backend: SystemWebviewBackend::WebView2,
        preferred_mode,
        imported_texture,
        native_child_overlay: CapabilityStatus::Supported,
        cpu_snapshot: CapabilityStatus::Supported,
        supported_frames: vec![NativeFrameKind::Dx12SharedTexture],
        reason: "Windows target path is WebView2 CompositionController visual capture into a D3D texture, then Dx12SharedTexture import.",
    }
}

/// A frame emitted by a Wry/system-webview producer.
#[non_exhaustive]
pub enum WryWebSurfaceFrame {
    Native(NativeFrame),
    CpuRgba {
        size: PhysicalSize<u32>,
        pixels: image::RgbaImage,
        generation: u64,
    },
    PngSnapshot {
        size: PhysicalSize<u32>,
        bytes: Vec<u8>,
        generation: u64,
    },
    OverlayOnly,
}

impl WryWebSurfaceFrame {
    pub fn mode(&self) -> WebSurfaceMode {
        match self {
            Self::Native(_) => WebSurfaceMode::ImportedTexture,
            Self::CpuRgba { .. } | Self::PngSnapshot { .. } => WebSurfaceMode::CpuSnapshot,
            Self::OverlayOnly => WebSurfaceMode::NativeChildOverlay,
        }
    }
}

#[derive(Debug, Error)]
pub enum WryWebSurfaceError {
    #[error("web surface mode is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("frame is not ready yet: {0}")]
    NotReady(&'static str),
    #[error(transparent)]
    Interop(#[from] InteropError),
    #[error("platform capture failed: {0}")]
    Platform(String),
}

/// Producer contract implemented by platform-specific Wry/WebView frame sources.
///
/// The trait covers the cross-platform lifecycle (capabilities + navigate +
/// resize + offset + a blocking acquire). Per-frame fast-path acquisition
/// and any platform-specific optimization signals (e.g. the Windows
/// "did the shared destination texture get re-allocated this frame"
/// flag) are exposed on the concrete platform producer types and not
/// on the trait, since they have no portable shape.
pub trait WryWebSurfaceProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities;

    fn mode(&self) -> WebSurfaceMode {
        self.capabilities().preferred_mode
    }

    /// Blocking acquire — returns the next available frame from the
    /// underlying capture path, possibly waiting for the WebView to
    /// produce one.
    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError>;

    /// Navigate the underlying WebView to inline HTML and block until
    /// `NavigationCompleted` (or analog) fires, or the timeout elapses.
    /// Producers that don't yet support navigation return
    /// [`WryWebSurfaceError::Unsupported`].
    fn navigate_to_string(
        &mut self,
        html: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = (html, timeout);
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::navigate_to_string is not implemented for this platform",
        ))
    }

    /// Resize the underlying WebView and capture region.
    fn resize(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = size;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::resize is not implemented for this platform",
        ))
    }

    /// Reposition the underlying WebView overlay relative to the parent
    /// host, in physical pixels.
    fn set_offset(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = (x, y);
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::set_offset is not implemented for this platform",
        ))
    }
}

/// Conservative overlay-only producer used when no capture backend is available yet.
#[derive(Clone, Debug)]
pub struct OverlayOnlyProducer {
    capabilities: WryWebSurfaceCapabilities,
}

impl OverlayOnlyProducer {
    pub fn new(capabilities: WryWebSurfaceCapabilities) -> Self {
        Self { capabilities }
    }
}

impl WryWebSurfaceProducer for OverlayOnlyProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        Ok(WryWebSurfaceFrame::OverlayOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_frame_reports_overlay_mode() {
        assert_eq!(
            WryWebSurfaceFrame::OverlayOnly.mode(),
            WebSurfaceMode::NativeChildOverlay
        );
    }

    #[test]
    fn unknown_host_on_windows_does_not_promise_imported_texture() {
        let caps = probe_webview2(None);
        assert_eq!(caps.backend, SystemWebviewBackend::WebView2);
        assert_eq!(caps.preferred_mode, WebSurfaceMode::NativeChildOverlay);
        assert_eq!(
            caps.imported_texture,
            CapabilityStatus::Unsupported(
                wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
            )
        );
    }
}
