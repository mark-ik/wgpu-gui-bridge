//! Windows WebView2 capture planning.
//!
//! The target path is WebView2 `ICoreWebView2CompositionController` plus
//! `Windows.Graphics.Capture`. Capture frames arrive as D3D11 textures; the
//! adapter must bridge them into a D3D12 shared texture before handing them to
//! `wgpu-native-texture-interop`.

use dpi::PhysicalSize;
use wgpu_native_texture_interop::{Dx12SharedTexture, NativeFrame, SyncMechanism};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HMODULE, HWND},
    Graphics::{
        Direct3D::{
            D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_11_1,
        },
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
            D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice,
            ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIDevice, IDXGIResource1,
        },
    },
    System::WinRT::{
        Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
        Graphics::Capture::IGraphicsCaptureItemInterop,
    },
};
use windows::{
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    core::{Interface, PCWSTR},
};

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

/// Owns a D3D11 device that can allocate NT-handle-shareable textures.
///
/// This is not the final WebView2 capture producer. It is the reusable helper
/// the producer needs once it receives `Direct3D11CaptureFrame.Surface` from
/// `Windows.Graphics.Capture`: either export a compatible capture texture
/// directly or copy the capture texture into a texture allocated here.
#[derive(Clone, Debug)]
pub struct D3D11SharedTextureFactory {
    device: ID3D11Device,
    #[allow(dead_code)]
    context: ID3D11DeviceContext,
}

impl D3D11SharedTextureFactory {
    pub fn new_hardware() -> Result<Self, WryWebSurfaceError> {
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
        }
        .map_err(|error| {
            WryWebSurfaceError::Platform(format!("D3D11CreateDevice failed: {error}"))
        })?;

        Ok(Self {
            device: device.ok_or_else(|| {
                WryWebSurfaceError::Platform("D3D11CreateDevice returned no device".to_string())
            })?,
            context: context.ok_or_else(|| {
                WryWebSurfaceError::Platform(
                    "D3D11CreateDevice returned no immediate context".to_string(),
                )
            })?,
        })
    }

    pub fn create_shared_texture_frame(
        &self,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        generation: u64,
    ) -> Result<WebView2DxgiSharedHandleFrame, WryWebSurfaceError> {
        Ok(self
            .create_shared_texture(size, format, generation)?
            .shared_frame)
    }

    fn create_shared_texture(
        &self,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        generation: u64,
    ) -> Result<D3D11SharedTexture, WryWebSurfaceError> {
        let dxgi_format = dxgi_format_for_wgpu(format)?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.width,
            Height: size.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: dxgi_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0
                | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32,
        };

        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(
            |error| WryWebSurfaceError::Platform(format!("CreateTexture2D failed: {error}")),
        )?;

        let texture = texture.ok_or_else(|| {
            WryWebSurfaceError::Platform("CreateTexture2D returned no texture".to_string())
        })?;

        let shared_frame = shared_handle_from_texture(&texture, size, format, generation)?;
        Ok(D3D11SharedTexture {
            texture,
            shared_frame,
        })
    }

    pub fn create_winrt_direct3d_device(&self) -> Result<IDirect3DDevice, WryWebSurfaceError> {
        let dxgi_device = self.device.cast::<IDXGIDevice>().map_err(|error| {
            WryWebSurfaceError::Platform(format!(
                "ID3D11Device cast to IDXGIDevice failed: {error}"
            ))
        })?;
        let inspectable =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) }.map_err(|error| {
                WryWebSurfaceError::Platform(format!(
                    "CreateDirect3D11DeviceFromDXGIDevice failed: {error}"
                ))
            })?;
        inspectable.cast::<IDirect3DDevice>().map_err(|error| {
            WryWebSurfaceError::Platform(format!("IDirect3DDevice cast failed: {error}"))
        })
    }

    pub fn copy_capture_into_shared_frame(
        &self,
        capture: WebView2D3D11CaptureFrame,
    ) -> Result<WebView2DxgiSharedHandleFrame, WryWebSurfaceError> {
        let target =
            self.create_shared_texture(capture.size, capture.format, capture.generation)?;

        with_borrowed_d3d11_texture(capture.raw_d3d11_texture, |source| {
            unsafe {
                self.context.CopyResource(&target.texture, source);
            }
            Ok(())
        })?;

        Ok(target.shared_frame)
    }
}

#[derive(Debug)]
struct D3D11SharedTexture {
    texture: ID3D11Texture2D,
    shared_frame: WebView2DxgiSharedHandleFrame,
}

/// Result of probing the Windows.Graphics.Capture side of the pipeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsCaptureProbe {
    pub session_supported: bool,
    pub winrt_d3d_device_created: bool,
    pub free_threaded_frame_pool_created: bool,
}

pub fn probe_graphics_capture_prerequisites() -> Result<GraphicsCaptureProbe, WryWebSurfaceError> {
    let session_supported = GraphicsCaptureSession::IsSupported().map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "GraphicsCaptureSession::IsSupported failed: {error}"
        ))
    })?;
    if !session_supported {
        return Ok(GraphicsCaptureProbe {
            session_supported,
            winrt_d3d_device_created: false,
            free_threaded_frame_pool_created: false,
        });
    }

    let factory = D3D11SharedTextureFactory::new_hardware()?;
    let device = factory.create_winrt_direct3d_device()?;
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        SizeInt32 {
            Width: 64,
            Height: 64,
        },
    )
    .map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "Direct3D11CaptureFramePool::CreateFreeThreaded failed: {error}"
        ))
    })?;
    drop(frame_pool);

    Ok(GraphicsCaptureProbe {
        session_supported,
        winrt_d3d_device_created: true,
        free_threaded_frame_pool_created: true,
    })
}

#[derive(Clone, Debug)]
pub struct CapturedWindowFrame {
    pub shared_frame: WebView2DxgiSharedHandleFrame,
    pub content_size: PhysicalSize<u32>,
}

/// Capture one frame from a HWND using Windows.Graphics.Capture.
///
/// This is a stand-in for the WebView2 CompositionController visual path. It
/// proves the downstream frame-pool and D3D11 texture extraction machinery
/// before we substitute `GraphicsCaptureItem::CreateFromVisual`.
///
/// # Safety
///
/// `hwnd` must be a valid live window handle for the duration of the call.
pub unsafe fn capture_window_frame_once(
    hwnd: *mut std::ffi::c_void,
    timeout: std::time::Duration,
) -> Result<CapturedWindowFrame, WryWebSurfaceError> {
    if hwnd.is_null() {
        return Err(WryWebSurfaceError::Platform(
            "window capture HWND was null".to_string(),
        ));
    }

    let session_supported = GraphicsCaptureSession::IsSupported().map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "GraphicsCaptureSession::IsSupported failed: {error}"
        ))
    })?;
    if !session_supported {
        return Err(WryWebSurfaceError::Unsupported(
            "Windows.Graphics.Capture is not supported in this session",
        ));
    }

    let item = create_capture_item_for_hwnd(HWND(hwnd))?;
    let item_size = item.Size().map_err(|error| {
        WryWebSurfaceError::Platform(format!("GraphicsCaptureItem::Size failed: {error}"))
    })?;
    if item_size.Width <= 0 || item_size.Height <= 0 {
        return Err(WryWebSurfaceError::Platform(format!(
            "GraphicsCaptureItem returned invalid size {}x{}",
            item_size.Width, item_size.Height
        )));
    }

    let factory = D3D11SharedTextureFactory::new_hardware()?;
    let device = factory.create_winrt_direct3d_device()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        item_size,
    )
    .map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "Direct3D11CaptureFramePool::CreateFreeThreaded failed: {error}"
        ))
    })?;
    let session = pool.CreateCaptureSession(&item).map_err(|error| {
        WryWebSurfaceError::Platform(format!("CreateCaptureSession failed: {error}"))
    })?;
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIsBorderRequired(false);
    session
        .StartCapture()
        .map_err(|error| WryWebSurfaceError::Platform(format!("StartCapture failed: {error}")))?;

    let deadline = std::time::Instant::now() + timeout;
    let frame = loop {
        match pool.TryGetNextFrame() {
            Ok(frame) => break frame,
            Err(last_error) if std::time::Instant::now() < deadline => {
                let _ = last_error;
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            Err(error) => {
                let _ = session.Close();
                let _ = pool.Close();
                return Err(WryWebSurfaceError::Platform(format!(
                    "TryGetNextFrame failed before timeout: {error}"
                )));
            }
        }
    };

    let content_size = frame.ContentSize().map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "Direct3D11CaptureFrame::ContentSize failed: {error}"
        ))
    })?;
    let surface = frame.Surface().map_err(|error| {
        WryWebSurfaceError::Platform(format!("Direct3D11CaptureFrame::Surface failed: {error}"))
    })?;
    let access = surface
        .cast::<IDirect3DDxgiInterfaceAccess>()
        .map_err(|error| {
            WryWebSurfaceError::Platform(format!(
                "IDirect3DSurface cast to IDirect3DDxgiInterfaceAccess failed: {error}"
            ))
        })?;
    let texture = unsafe { access.GetInterface::<ID3D11Texture2D>() }.map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "IDirect3DDxgiInterfaceAccess::GetInterface<ID3D11Texture2D> failed: {error}"
        ))
    })?;

    let raw_texture = Interface::as_raw(&texture);
    let shared_frame = factory.copy_capture_into_shared_frame(WebView2D3D11CaptureFrame {
        size: PhysicalSize::new(content_size.Width as u32, content_size.Height as u32),
        format: wgpu::TextureFormat::Bgra8Unorm,
        generation: 1,
        raw_d3d11_texture: raw_texture,
    })?;

    let _ = frame.Close();
    let _ = session.Close();
    let _ = pool.Close();

    Ok(CapturedWindowFrame {
        shared_frame,
        content_size: PhysicalSize::new(content_size.Width as u32, content_size.Height as u32),
    })
}

fn create_capture_item_for_hwnd(hwnd: HWND) -> Result<GraphicsCaptureItem, WryWebSurfaceError> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|error| {
            WryWebSurfaceError::Platform(format!(
                "GraphicsCaptureItem interop factory failed: {error}"
            ))
        })?;
    unsafe { interop.CreateForWindow::<GraphicsCaptureItem>(hwnd) }.map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "IGraphicsCaptureItemInterop::CreateForWindow failed: {error}"
        ))
    })
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

/// Close an NT shared handle returned by this module after the consumer has
/// opened its own resource reference.
///
/// # Safety
///
/// `handle` must be a valid Win32 handle owned by the caller, and it must not
/// be used after this call succeeds.
pub unsafe fn close_shared_handle(handle: *mut std::ffi::c_void) -> Result<(), WryWebSurfaceError> {
    if handle.is_null() {
        return Ok(());
    }

    unsafe { CloseHandle(HANDLE(handle)) }
        .map_err(|error| WryWebSurfaceError::Platform(format!("CloseHandle failed: {error}")))
}

pub fn export_capture_frame_shared_handle(
    frame: WebView2D3D11CaptureFrame,
) -> Result<WebView2DxgiSharedHandleFrame, WryWebSurfaceError> {
    with_borrowed_d3d11_texture(frame.raw_d3d11_texture, |texture| {
        shared_handle_from_texture(texture, frame.size, frame.format, frame.generation)
    })
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

impl D3D11ToDx12Bridge for DxgiSharedHandleBridge {
    fn bridge_frame(
        &self,
        frame: WebView2D3D11CaptureFrame,
    ) -> Result<WebView2Dx12SharedFrame, WryWebSurfaceError> {
        self.bridge_shared_handle(export_capture_frame_shared_handle(frame)?)
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

fn dxgi_format_for_wgpu(
    format: wgpu::TextureFormat,
) -> Result<windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT, WryWebSurfaceError> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => Ok(DXGI_FORMAT_R8G8B8A8_UNORM),
        wgpu::TextureFormat::Bgra8Unorm => Ok(DXGI_FORMAT_B8G8R8A8_UNORM),
        _ => Err(WryWebSurfaceError::Unsupported(
            "only Rgba8Unorm and Bgra8Unorm D3D11 capture textures are supported",
        )),
    }
}

fn with_borrowed_d3d11_texture<R>(
    raw: *mut std::ffi::c_void,
    f: impl FnOnce(&ID3D11Texture2D) -> Result<R, WryWebSurfaceError>,
) -> Result<R, WryWebSurfaceError> {
    if raw.is_null() {
        return Err(WryWebSurfaceError::Platform(
            "D3D11 capture texture pointer was null".to_string(),
        ));
    }

    unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }
        .ok_or_else(|| {
            WryWebSurfaceError::Platform("failed to borrow ID3D11Texture2D pointer".to_string())
        })
        .and_then(f)
}

fn shared_handle_from_texture(
    texture: &ID3D11Texture2D,
    size: PhysicalSize<u32>,
    format: wgpu::TextureFormat,
    generation: u64,
) -> Result<WebView2DxgiSharedHandleFrame, WryWebSurfaceError> {
    let resource = texture.cast::<IDXGIResource1>().map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "ID3D11Texture2D cast to IDXGIResource1 failed: {error}"
        ))
    })?;

    let handle = unsafe {
        resource.CreateSharedHandle(
            None,
            (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
            PCWSTR::null(),
        )
    }
    .map_err(|error| {
        WryWebSurfaceError::Platform(format!(
            "IDXGIResource1::CreateSharedHandle failed: {error}"
        ))
    })?;

    Ok(WebView2DxgiSharedHandleFrame {
        size,
        format,
        generation,
        shared_handle: handle.0 as *mut std::ffi::c_void,
    })
}
