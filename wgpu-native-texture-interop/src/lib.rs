#![doc = include_str!("../README.md")]

mod error;
mod sync;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
mod gl_bindings {
    #![allow(unsafe_op_in_unsafe_fn)]

    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

pub mod raw_gl;

#[cfg(feature = "surfman")]
pub mod surfman_gl;

use std::rc::Rc;

pub use error::{InteropError, UnsupportedReason};
pub use sync::{ImplicitOnlySynchronizer, InteropSynchronizer, NoopSynchronizer, SyncMechanism};
use dpi::PhysicalSize;

/// The wgpu graphics backend in use on the host device.
///
/// Detected automatically by [`HostWgpuContext::new`] via `as_hal`. Used to
/// drive [`CapabilityMatrix::for_backend`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteropBackend {
    /// Vulkan backend (Linux, Android, Windows with `wgpu::Backends::VULKAN`).
    Vulkan,
    /// Metal backend (macOS, iOS).
    Metal,
    /// Direct3D 12 backend (Windows default). GL→DX12 import is not yet
    /// implemented; use `wgpu::Backends::VULKAN` on Windows for GL interop.
    Dx12,
    /// Backend could not be detected. All import paths will report
    /// [`CapabilityStatus::Unsupported`].
    Unknown,
}

/// Which corner of the texture holds row 0 of the image.
///
/// GL renders with the origin at the bottom-left; most compositors expect
/// top-left. The import paths in this crate Y-flip during blit so that all
/// returned textures have [`TextureOrigin::TopLeft`] when
/// [`ImportOptions::normalize_origin`] is `true` (the default).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureOrigin {
    /// Row 0 is the top row. The standard convention for wgpu/Vulkan/Metal.
    TopLeft,
    /// Row 0 is the bottom row. Raw GL output before Y-flip normalization.
    BottomLeft,
}

/// Discriminant for [`NativeFrame`] variants, without carrying the frame data.
///
/// Returned by [`NativeFrame::kind`] and used in [`ProducerCapabilities`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeFrameKind {
    /// A GL framebuffer that will be imported via the platform-specific path.
    GlFramebufferSource,
    /// A Vulkan external image. Import not yet implemented.
    VulkanExternalImage,
    /// A Metal texture reference. Import not yet implemented.
    MetalTextureRef,
    /// A D3D12 shared texture. Import not yet implemented.
    Dx12SharedTexture,
}

/// Whether a particular interop capability is available on this device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    /// The capability is available and `import_frame` should succeed.
    Supported,
    /// The capability is not available for the given reason.
    Unsupported(UnsupportedReason),
}

/// Reports which frame types can be imported on the current device and backend.
///
/// Obtain via [`HostWgpuContext::capabilities`] or
/// [`CapabilityMatrix::for_backend`]. Use this before attempting an import to
/// give the user an early, descriptive error rather than a runtime failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityMatrix {
    /// The backend detected on the host wgpu device.
    pub host_backend: InteropBackend,
    /// GL framebuffer import (the primary path — Linux Vulkan, Apple Metal).
    pub gl_framebuffer_source: CapabilityStatus,
    /// Direct Vulkan external image import. Not yet implemented.
    pub vulkan_external_image: CapabilityStatus,
    /// Direct Metal texture reference import. Not yet implemented.
    pub metal_texture_ref: CapabilityStatus,
    /// D3D12 shared texture import. Not yet implemented.
    pub dx12_shared_texture: CapabilityStatus,
}

/// The set of [`NativeFrameKind`]s a [`FrameProducer`] is able to emit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProducerCapabilities {
    /// Frame kinds this producer can supply.
    pub supported_frames: Vec<NativeFrameKind>,
}

/// Wraps a `wgpu::Device` and `wgpu::Queue` together with the detected backend.
///
/// Pass one of these to [`WgpuTextureImporter::new`] or directly to the
/// platform-specific import functions.
#[derive(Clone, Debug)]
pub struct HostWgpuContext {
    /// The wgpu device that will own imported textures.
    pub device: wgpu::Device,
    /// The queue associated with `device`.
    pub queue: wgpu::Queue,
    /// The graphics backend detected on `device` at construction time.
    pub backend: InteropBackend,
}

impl HostWgpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            backend: detect_backend(&device),
            device,
            queue,
        }
    }

    pub fn capabilities(&self) -> CapabilityMatrix {
        CapabilityMatrix::for_backend(self.backend)
    }
}

/// Options that control how [`WgpuTextureImporter`] processes each frame.
#[derive(Clone, Copy, Debug)]
pub struct ImportOptions {
    /// If `true`, fall back to a CPU-side copy when the zero-copy path is
    /// unavailable. Currently unused — reserved for future use.
    pub allow_copy_fallback: bool,
    /// If `true` (default), the importer runs a GPU blit/shader pass to
    /// flip the texture to [`TextureOrigin::TopLeft`]. Set to `false` only
    /// if you want the raw GL bottom-left orientation.
    pub normalize_origin: bool,
    /// If `true` (default), the importer converts BGRA output (Apple) to
    /// RGBA so that all returned textures have a consistent
    /// `Rgba8Unorm` format.
    pub normalize_format: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            allow_copy_fallback: false,
            normalize_origin: true,
            normalize_format: true,
        }
    }
}

/// A successfully imported wgpu texture, ready for use in a render pipeline.
///
/// Returned by [`TextureImporter::import_frame`].
#[derive(Debug)]
pub struct ImportedTexture {
    /// The imported wgpu texture. Bind this as a texture resource in your
    /// render pipeline.
    pub texture: wgpu::Texture,
    /// The pixel format of `texture`. `Rgba8Unorm` when
    /// [`ImportOptions::normalize_format`] is `true` (the default).
    pub format: wgpu::TextureFormat,
    /// Dimensions of `texture` in physical pixels.
    pub size: PhysicalSize<u32>,
    /// Whether row 0 of `texture` is the top or bottom of the image.
    /// [`TextureOrigin::TopLeft`] when [`ImportOptions::normalize_origin`]
    /// is `true` (the default).
    pub origin: TextureOrigin,
    /// Monotonically increasing counter that the producer increments each
    /// time new content is rendered. Use this to skip redundant re-imports.
    pub generation: u64,
    /// The synchronization mechanism the consumer should use after reading
    /// `texture`. Passed to [`InteropSynchronizer::consumer_ready`].
    pub consumer_sync: SyncMechanism,
}

pub struct GlFramebufferSource {
    size: PhysicalSize<u32>,
    generation: u64,
    producer_sync: SyncMechanism,
    importer: Rc<dyn GlFramebufferSourceImpl>,
}

impl GlFramebufferSource {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn producer_sync(&self) -> SyncMechanism {
        self.producer_sync
    }

    pub fn new(
        size: PhysicalSize<u32>,
        generation: u64,
        producer_sync: SyncMechanism,
        importer: Rc<dyn GlFramebufferSourceImpl>,
    ) -> Self {
        Self {
            size,
            generation,
            producer_sync,
            importer,
        }
    }
}

/// Metadata for a Vulkan external image frame. **Import not yet implemented.**
#[derive(Clone, Copy, Debug)]
pub struct VulkanExternalImage {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    pub producer_sync: SyncMechanism,
}

/// A frame backed by a `MTLTexture` from a Metal producer.
///
/// The producer is responsible for ensuring the texture remains valid for the
/// duration of the import call. Ownership is **not** transferred; the importer
/// wraps the texture without retaining it via Objective-C ARC.
#[derive(Clone, Copy, Debug)]
pub struct MetalTextureRef {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    pub producer_sync: SyncMechanism,
    /// Raw `MTLTexture *` pointer. Must be non-null. Apple platforms only.
    ///
    /// The caller retains ownership and must ensure the texture outlives this
    /// struct. The importer does not call `retain` or `release` on the pointer.
    #[cfg(target_vendor = "apple")]
    pub raw_metal_texture: *mut std::ffi::c_void,
}

/// A frame backed by a D3D12 resource shared via a DXGI NT handle.
///
/// Obtain the handle by calling `IDXGIResource1::CreateSharedHandle` on your
/// `ID3D12Resource`. The importer opens its own D3D12 reference via
/// `ID3D12Device::OpenSharedHandle`; **you are responsible for closing your
/// copy** of the handle after constructing this struct.
#[derive(Clone, Copy, Debug)]
pub struct Dx12SharedTexture {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    pub producer_sync: SyncMechanism,
    /// NT `HANDLE` from `IDXGIResource1::CreateSharedHandle`. Windows only.
    ///
    /// The importer opens its own reference via `OpenSharedHandle`. Close
    /// your copy of the handle once this struct has been passed to
    /// [`TextureImporter::import_frame`].
    #[cfg(target_os = "windows")]
    pub handle: *mut std::ffi::c_void,
}

/// A frame produced by a [`FrameProducer`], ready to be imported by a
/// [`TextureImporter`].
///
/// A frame produced by a [`FrameProducer`], ready to be imported by a
/// [`TextureImporter`].
///
/// `GlFramebufferSource`, `MetalTextureRef`, and `Dx12SharedTexture` have
/// complete import implementations. `VulkanExternalImage` is defined for API
/// stability but its import path is not yet implemented.
#[non_exhaustive]
pub enum NativeFrame {
    /// A GL framebuffer — the primary, fully-implemented path.
    GlFramebufferSource(GlFramebufferSource),
    /// A Vulkan external image. Not yet implemented — returns
    /// [`UnsupportedReason::NativeImportNotYetImplemented`].
    VulkanExternalImage(VulkanExternalImage),
    /// A Metal texture reference. Fully implemented via IOSurface interop.
    MetalTextureRef(MetalTextureRef),
    /// A D3D12 shared texture. Fully implemented via shared handle interop.
    Dx12SharedTexture(Dx12SharedTexture),
}

impl NativeFrame {
    pub fn kind(&self) -> NativeFrameKind {
        match self {
            NativeFrame::GlFramebufferSource(_) => NativeFrameKind::GlFramebufferSource,
            NativeFrame::VulkanExternalImage(_) => NativeFrameKind::VulkanExternalImage,
            NativeFrame::MetalTextureRef(_) => NativeFrameKind::MetalTextureRef,
            NativeFrame::Dx12SharedTexture(_) => NativeFrameKind::Dx12SharedTexture,
        }
    }

    pub fn producer_sync(&self) -> SyncMechanism {
        match self {
            NativeFrame::GlFramebufferSource(frame) => frame.producer_sync(),
            NativeFrame::VulkanExternalImage(frame) => frame.producer_sync,
            NativeFrame::MetalTextureRef(frame) => frame.producer_sync,
            NativeFrame::Dx12SharedTexture(frame) => frame.producer_sync,
        }
    }
}

/// Produces [`NativeFrame`]s for a [`TextureImporter`] to consume.
///
/// Implement this for your GL/Vulkan/Metal renderer to feed frames into the
/// interop pipeline. See [`raw_gl::producer::RawGlFrameProducer`] for a
/// ready-made implementation that wraps any GL context.
pub trait FrameProducer {
    /// Returns what frame kinds this producer can emit.
    fn capabilities(&self) -> ProducerCapabilities;
    /// Acquire the next frame from the producer. The returned [`NativeFrame`]
    /// should be passed immediately to [`TextureImporter::import_frame`].
    fn acquire_frame(&mut self) -> Result<NativeFrame, InteropError>;
}

/// Imports a [`NativeFrame`] into a `wgpu::Texture`.
pub trait TextureImporter {
    /// Import `frame` into a [`wgpu::Texture`] owned by the host device.
    ///
    /// Returns [`InteropError::Unsupported`] if the frame kind is not
    /// supported on the current platform/backend. Check
    /// [`HostWgpuContext::capabilities`] first to get a descriptive error
    /// before calling this.
    fn import_frame(
        &self,
        frame: &NativeFrame,
        options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError>;
}

/// The main entry point for importing frames into wgpu textures.
///
/// Create one per wgpu device and reuse it across frames.
///
/// ```ignore
/// let host = HostWgpuContext::new(device, queue);
/// let importer = WgpuTextureImporter::new(host);
/// // each frame:
/// let frame = producer.acquire_frame()?;
/// let imported = importer.import_frame(&frame, &ImportOptions::default())?;
/// // use imported.texture in your render pipeline
/// ```
pub struct WgpuTextureImporter {
    host: HostWgpuContext,
    synchronizer: Box<dyn InteropSynchronizer>,
}

impl WgpuTextureImporter {
    /// Create a new importer with the default [`ImplicitOnlySynchronizer`].
    pub fn new(host: HostWgpuContext) -> Self {
        Self {
            host,
            synchronizer: Box::new(ImplicitOnlySynchronizer),
        }
    }

    /// Create a new importer with a custom [`InteropSynchronizer`].
    pub fn with_synchronizer(
        host: HostWgpuContext,
        synchronizer: Box<dyn InteropSynchronizer>,
    ) -> Self {
        Self { host, synchronizer }
    }

    /// Returns the underlying [`HostWgpuContext`].
    pub fn host(&self) -> &HostWgpuContext {
        &self.host
    }
}

impl TextureImporter for WgpuTextureImporter {
    fn import_frame(
        &self,
        frame: &NativeFrame,
        options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError> {
        self.synchronizer
            .producer_complete(frame, frame.producer_sync())?;

        let imported =
            match frame {
                NativeFrame::GlFramebufferSource(frame_source) => frame_source
                    .importer
                    .import_into(frame_source, &self.host, options),
                NativeFrame::VulkanExternalImage(_) => Err(InteropError::Unsupported(
                    UnsupportedReason::NativeImportNotYetImplemented,
                )),
                NativeFrame::MetalTextureRef(frame) => {
                    import_metal_texture_ref(frame, &self.host)
                }
                NativeFrame::Dx12SharedTexture(frame) => {
                    import_dx12_shared_texture(frame, &self.host)
                }
            }?;

        self.synchronizer
            .consumer_ready(&imported, imported.consumer_sync)?;
        Ok(imported)
    }
}

impl CapabilityMatrix {
    pub fn for_backend(host_backend: InteropBackend) -> Self {
        let gl_framebuffer_source = match host_backend {
            InteropBackend::Vulkan | InteropBackend::Metal | InteropBackend::Dx12 => {
                CapabilityStatus::Supported
            }
            InteropBackend::Unknown => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendUnavailable)
            }
        };

        let vulkan_external_image = match host_backend {
            InteropBackend::Vulkan => {
                CapabilityStatus::Unsupported(UnsupportedReason::NativeImportNotYetImplemented)
            }
            InteropBackend::Metal | InteropBackend::Dx12 => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
            }
            InteropBackend::Unknown => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendUnavailable)
            }
        };

        let metal_texture_ref = match host_backend {
            InteropBackend::Metal => CapabilityStatus::Supported,
            InteropBackend::Vulkan | InteropBackend::Dx12 => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
            }
            InteropBackend::Unknown => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendUnavailable)
            }
        };

        let dx12_shared_texture = match host_backend {
            InteropBackend::Dx12 => CapabilityStatus::Supported,
            InteropBackend::Vulkan | InteropBackend::Metal => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
            }
            InteropBackend::Unknown => {
                CapabilityStatus::Unsupported(UnsupportedReason::HostBackendUnavailable)
            }
        };

        Self {
            host_backend,
            gl_framebuffer_source,
            vulkan_external_image,
            metal_texture_ref,
            dx12_shared_texture,
        }
    }
}

pub trait GlFramebufferSourceImpl {
    fn import_into(
        &self,
        frame: &GlFramebufferSource,
        host: &HostWgpuContext,
        options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError>;
}

fn import_metal_texture_ref(
    #[cfg_attr(not(target_vendor = "apple"), allow(unused_variables))]
    frame: &MetalTextureRef,
    #[cfg_attr(not(target_vendor = "apple"), allow(unused_variables))]
    host: &HostWgpuContext,
) -> Result<ImportedTexture, InteropError> {
    #[cfg(target_vendor = "apple")]
    {
        use foreign_types_shared::ForeignType;
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;

        if frame.raw_metal_texture.is_null() {
            return Err(InteropError::InvalidFrame("raw_metal_texture is null"));
        }
        if host.backend != InteropBackend::Metal {
            return Err(InteropError::BackendMismatch {
                expected: "Metal",
                actual: "non-Metal",
            });
        }

        let texture = unsafe {
            // Retain the caller's MTLTexture so that wgpu can take ownership
            // of the reference we hand it without invalidating the caller's copy.
            let obj_ptr = frame.raw_metal_texture as *mut AnyObject;
            let retained = Retained::retain(obj_ptr)
                .ok_or_else(|| InteropError::Metal("failed to retain Metal texture".into()))?;
            let raw_ptr = Retained::into_raw(retained) as *mut _;
            let metal_texture = metal::Texture::from_ptr(raw_ptr);

            // texture_from_raw is a free associated function — matches the
            // signature used in raw_gl/metal.rs.
            let hal_texture = wgpu::hal::metal::Device::texture_from_raw(
                metal_texture,
                frame.format,
                metal::MTLTextureType::D2,
                0, // array_layers
                0, // mip_levels
                wgpu::hal::CopyExtent {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth: 0,
                },
            );

            host.device
                .create_texture_from_hal::<wgpu::wgc::api::Metal>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("metal-texture-ref-import"),
                        size: wgpu::Extent3d {
                            width: frame.size.width,
                            height: frame.size.height,
                            depth_or_array_layers: 1,
                        },
                        format: frame.format,
                        dimension: wgpu::TextureDimension::D2,
                        mip_level_count: 1,
                        sample_count: 1,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                )
        };

        return Ok(ImportedTexture {
            texture,
            format: frame.format,
            size: frame.size,
            origin: TextureOrigin::TopLeft,
            generation: frame.generation,
            consumer_sync: frame.producer_sync,
        });
    }

    #[cfg(not(target_vendor = "apple"))]
    Err(InteropError::Unsupported(
        UnsupportedReason::HostBackendMismatch,
    ))
}

fn import_dx12_shared_texture(
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    frame: &Dx12SharedTexture,
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    host: &HostWgpuContext,
) -> Result<ImportedTexture, InteropError> {
    #[cfg(target_os = "windows")]
    {
        if host.backend != InteropBackend::Dx12 {
            return Err(InteropError::BackendMismatch {
                expected: "Dx12",
                actual: "non-Dx12",
            });
        }

        let texture = unsafe {
            let hal_device = host
                .device
                .as_hal::<wgpu::wgc::api::Dx12>()
                .ok_or(InteropError::BackendMismatch {
                    expected: "Dx12",
                    actual: "non-Dx12",
                })?;

            let d3d_device = hal_device.raw_device().clone();
            let mut resource: Option<windows::Win32::Graphics::Direct3D12::ID3D12Resource> = None;
            d3d_device
                .OpenSharedHandle(
                    windows::Win32::Foundation::HANDLE(frame.handle as *mut std::ffi::c_void),
                    &mut resource,
                )
                .map_err(|e| InteropError::Dx12(e.to_string()))?;
            let resource = resource
                .ok_or_else(|| InteropError::Dx12("OpenSharedHandle returned null".into()))?;

            let hal_texture = wgpu_hal::dx12::Device::texture_from_raw(
                resource,
                frame.format,
                wgpu::TextureDimension::D2,
                wgpu::Extent3d {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth_or_array_layers: 1,
                },
                1, // mip_level_count
                1, // sample_count
            );

            host.device
                .create_texture_from_hal::<wgpu::wgc::api::Dx12>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("dx12-shared-texture-import"),
                        size: wgpu::Extent3d {
                            width: frame.size.width,
                            height: frame.size.height,
                            depth_or_array_layers: 1,
                        },
                        format: frame.format,
                        dimension: wgpu::TextureDimension::D2,
                        mip_level_count: 1,
                        sample_count: 1,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                )
        };

        return Ok(ImportedTexture {
            texture,
            format: frame.format,
            size: frame.size,
            origin: TextureOrigin::TopLeft,
            generation: frame.generation,
            consumer_sync: frame.producer_sync,
        });
    }

    #[cfg(not(target_os = "windows"))]
    Err(InteropError::Unsupported(
        UnsupportedReason::HostBackendMismatch,
    ))
}

fn detect_backend(device: &wgpu::Device) -> InteropBackend {
    unsafe {
        // wgpu::wgc::api::Vulkan is only compiled in when the hal `vulkan` cfg
        // is set — i.e. Linux, Android, and Windows (not macOS).
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
        if device.as_hal::<wgpu::wgc::api::Vulkan>().is_some() {
            return InteropBackend::Vulkan;
        }

        #[cfg(target_vendor = "apple")]
        if device.as_hal::<wgpu::wgc::api::Metal>().is_some() {
            return InteropBackend::Metal;
        }

        #[cfg(target_os = "windows")]
        if device.as_hal::<wgpu::wgc::api::Dx12>().is_some() {
            return InteropBackend::Dx12;
        }
    }

    InteropBackend::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_options_default_prefers_normalized_textures() {
        let options = ImportOptions::default();

        assert!(!options.allow_copy_fallback);
        assert!(options.normalize_origin);
        assert!(options.normalize_format);
    }

    #[test]
    fn capability_matrix_tracks_backend_shape() {
        let vulkan = CapabilityMatrix::for_backend(InteropBackend::Vulkan);
        let metal = CapabilityMatrix::for_backend(InteropBackend::Metal);
        let dx12 = CapabilityMatrix::for_backend(InteropBackend::Dx12);
        let unknown = CapabilityMatrix::for_backend(InteropBackend::Unknown);

        assert_eq!(vulkan.gl_framebuffer_source, CapabilityStatus::Supported);
        assert_eq!(metal.gl_framebuffer_source, CapabilityStatus::Supported);
        assert_eq!(dx12.gl_framebuffer_source, CapabilityStatus::Supported);
        assert_eq!(
            unknown.gl_framebuffer_source,
            CapabilityStatus::Unsupported(UnsupportedReason::HostBackendUnavailable)
        );

        assert_eq!(
            vulkan.vulkan_external_image,
            CapabilityStatus::Unsupported(UnsupportedReason::NativeImportNotYetImplemented)
        );
        assert_eq!(
            metal.vulkan_external_image,
            CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
        );

        assert_eq!(metal.metal_texture_ref, CapabilityStatus::Supported);
        assert_eq!(
            vulkan.metal_texture_ref,
            CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
        );

        assert_eq!(dx12.dx12_shared_texture, CapabilityStatus::Supported);
        assert_eq!(
            vulkan.dx12_shared_texture,
            CapabilityStatus::Unsupported(UnsupportedReason::HostBackendMismatch)
        );
    }

    #[test]
    fn implicit_synchronizer_accepts_implicit_flush() {
        assert!(ImplicitOnlySynchronizer::validate(SyncMechanism::ImplicitGlFlush).is_ok());
    }

    #[test]
    fn implicit_synchronizer_rejects_explicit_sync() {
        assert!(matches!(
            ImplicitOnlySynchronizer::validate(SyncMechanism::ExplicitExternalSemaphore),
            Err(InteropError::UnsupportedSynchronization(
                SyncMechanism::ExplicitExternalSemaphore
            ))
        ));
    }
}
