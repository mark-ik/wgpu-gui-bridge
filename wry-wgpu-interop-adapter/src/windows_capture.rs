//! Windows WebView2 capture planning.
//!
//! The target path is WebView2 `ICoreWebView2CompositionController` plus
//! `Windows.Graphics.Capture`. Capture frames arrive as D3D11 textures; the
//! adapter must bridge them into a D3D12 shared texture before handing them to
//! `wgpu-native-texture-interop`.

use dpi::PhysicalSize;
use wgpu_native_texture_interop::{Dx12SharedTexture, NativeFrame, SyncMechanism};

use crate::{WryWebSurfaceError, WryWebSurfaceFrame};

/// Metadata for a captured WebView2 frame before it has been converted into a
/// `NativeFrame::Dx12SharedTexture`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebView2D3D11CaptureFrame {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    /// Raw `ID3D11Texture2D *`. The capture owner retains lifetime.
    pub raw_d3d11_texture: *mut std::ffi::c_void,
}

/// Result of converting a captured D3D11 frame into an importable D3D12 frame.
#[derive(Clone, Copy, Debug)]
pub struct WebView2Dx12SharedFrame {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    /// NT shared handle suitable for `ID3D12Device::OpenSharedHandle`.
    pub shared_handle: *mut std::ffi::c_void,
}

impl WebView2Dx12SharedFrame {
    pub fn into_surface_frame(self) -> WryWebSurfaceFrame {
        WryWebSurfaceFrame::Native(NativeFrame::Dx12SharedTexture(Dx12SharedTexture {
            size: self.size,
            format: self.format,
            generation: self.generation,
            producer_sync: SyncMechanism::None,
            handle: self.shared_handle,
        }))
    }
}

/// A capture frame that already has a DXGI/D3D shared handle.
///
/// This is the narrow handoff shape the WebView2 capture implementation should
/// try to reach after receiving a `Direct3D11CaptureFrame`. If the captured
/// `ID3D11Texture2D` can expose a handle that `ID3D12Device::OpenSharedHandle`
/// accepts, no CPU readback is needed.
#[derive(Clone, Copy, Debug)]
pub struct WebView2DxgiSharedHandleFrame {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    /// NT shared handle. The caller remains responsible for closing its copy.
    pub shared_handle: *mut std::ffi::c_void,
}

impl WebView2DxgiSharedHandleFrame {
    pub fn into_dx12_frame(self) -> WebView2Dx12SharedFrame {
        WebView2Dx12SharedFrame {
            size: self.size,
            format: self.format,
            generation: self.generation,
            shared_handle: self.shared_handle,
        }
    }

    pub fn into_surface_frame(self) -> WryWebSurfaceFrame {
        self.into_dx12_frame().into_surface_frame()
    }
}

/// Describes the Windows proof path without owning COM/WinRT objects yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebView2CompositionCapturePlan {
    pub requires_composition_controller: bool,
    pub requires_graphics_capture_item_from_visual: bool,
    pub capture_texture_api: &'static str,
    pub import_texture_kind: &'static str,
}

impl Default for WebView2CompositionCapturePlan {
    fn default() -> Self {
        Self {
            requires_composition_controller: true,
            requires_graphics_capture_item_from_visual: true,
            capture_texture_api: "Windows.Graphics.Capture.Direct3D11CaptureFrame.Surface",
            import_texture_kind: "NativeFrame::Dx12SharedTexture",
        }
    }
}

/// Placeholder bridge for the first hard proof point.
///
/// The implementation must prove either D3D11 shared-handle import into D3D12
/// or a D3D11On12 copy into a D3D12 shared resource before the adapter can
/// honestly advertise interactive `ImportedTexture` support.
pub trait D3D11ToDx12Bridge {
    fn bridge_frame(
        &self,
        frame: WebView2D3D11CaptureFrame,
    ) -> Result<WebView2Dx12SharedFrame, WryWebSurfaceError>;
}

/// Bridge implementation for capture paths that can already produce a
/// D3D12-openable DXGI shared handle.
#[derive(Clone, Debug, Default)]
pub struct DxgiSharedHandleBridge;

impl DxgiSharedHandleBridge {
    pub fn bridge_shared_handle(
        &self,
        frame: WebView2DxgiSharedHandleFrame,
    ) -> Result<WebView2Dx12SharedFrame, WryWebSurfaceError> {
        if frame.shared_handle.is_null() {
            return Err(WryWebSurfaceError::Platform(
                "WebView2 capture shared handle was null".to_string(),
            ));
        }
        Ok(frame.into_dx12_frame())
    }
}

#[derive(Clone, Debug, Default)]
pub struct UnsupportedD3D11ToDx12Bridge;

impl D3D11ToDx12Bridge for UnsupportedD3D11ToDx12Bridge {
    fn bridge_frame(
        &self,
        _frame: WebView2D3D11CaptureFrame,
    ) -> Result<WebView2Dx12SharedFrame, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "D3D11 capture texture to D3D12 shared texture bridge is not implemented yet",
        ))
    }
}
