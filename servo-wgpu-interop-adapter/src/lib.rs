#![doc = include_str!("../README.md")]

use std::{cell::RefCell, rc::Rc};

use euclid::default::Size2D;
use surfman::{Connection, Device, Surface, SurfaceType, chains::SwapChain};
use thiserror::Error;
use wgpu_native_texture_interop::{
    FrameProducer, HostWgpuContext, ImportOptions, ImportedTexture, InteropError, TextureImporter,
    WgpuTextureImporter,
    surfman_gl::{SurfmanFrameContext, SurfmanFrameProducer},
};
use winit::dpi::PhysicalSize;

#[cfg(feature = "servo")]
pub use image;
#[cfg(feature = "servo")]
use servo::{DeviceIntRect, RenderingContext};
#[cfg(feature = "servo")]
use surfman::{SurfaceTexture, chains::PreserveBuffer};

pub use wgpu_native_texture_interop::{
    ImportOptions as InteropImportOptions, ImportedTexture as InteropImportedTexture,
};

pub struct ImportedSurfmanSurface {
    pub imported_texture: ImportedTexture,
    pub surface: Surface,
}

#[derive(Debug, Error)]
pub enum SurfmanSurfaceImportError {
    #[error("failed to bind surfman surface")]
    BindSurface(surfman::Error),
    #[error("failed to make surfman context current")]
    MakeCurrent(surfman::Error),
    #[error("failed to acquire frame from surfman context")]
    AcquireFrame(#[source] InteropError),
    #[error("failed to import frame into wgpu")]
    ImportFrame(#[source] InteropError),
    #[error("failed to unbind surfman surface")]
    UnbindSurface(surfman::Error),
    #[error("import completed without returning a surfman surface")]
    MissingSurfaceAfterImport,
}

#[derive(Debug)]
pub struct SurfmanSurfaceImportFailure {
    error: SurfmanSurfaceImportError,
    surface: Option<Surface>,
}

impl SurfmanSurfaceImportFailure {
    pub fn error(&self) -> &SurfmanSurfaceImportError {
        &self.error
    }

    pub fn into_parts(self) -> (SurfmanSurfaceImportError, Option<Surface>) {
        (self.error, self.surface)
    }
}

pub struct SurfmanSurfaceImporter {
    frame_context: Rc<SurfmanFrameContext>,
    importer: WgpuTextureImporter,
}

impl SurfmanSurfaceImporter {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, surfman::Error> {
        let connection = Connection::new()?;
        let adapter = connection.create_adapter()?;
        let frame_context = Rc::new(SurfmanFrameContext::new(&connection, &adapter)?);
        let importer = WgpuTextureImporter::new(HostWgpuContext::new(device, queue));

        Ok(Self {
            frame_context,
            importer,
        })
    }

    pub fn import_surface(
        &self,
        surface: Surface,
        options: &ImportOptions,
    ) -> Result<ImportedSurfmanSurface, SurfmanSurfaceImportFailure> {
        let size = {
            let device = self.frame_context.device.borrow();
            let info = device.surface_info(&surface);
            PhysicalSize::new(info.size.width as u32, info.size.height as u32)
        };

        if let Err(error) = self.frame_context.bind_surface(surface) {
            return Err(SurfmanSurfaceImportFailure {
                error: SurfmanSurfaceImportError::BindSurface(error),
                surface: None,
            });
        }

        if let Err(error) = self.frame_context.make_current() {
            return Err(SurfmanSurfaceImportFailure {
                error: SurfmanSurfaceImportError::MakeCurrent(error),
                surface: self.frame_context.unbind_surface().ok().flatten(),
            });
        }

        let mut producer = SurfmanFrameProducer::new(self.frame_context.clone(), size);
        let frame = match producer.acquire_frame() {
            Ok(frame) => frame,
            Err(error) => {
                return Err(SurfmanSurfaceImportFailure {
                    error: SurfmanSurfaceImportError::AcquireFrame(error),
                    surface: self.frame_context.unbind_surface().ok().flatten(),
                });
            },
        };

        let imported_texture = match self.importer.import_frame(&frame, options) {
            Ok(texture) => texture,
            Err(error) => {
                return Err(SurfmanSurfaceImportFailure {
                    error: SurfmanSurfaceImportError::ImportFrame(error),
                    surface: self.frame_context.unbind_surface().ok().flatten(),
                });
            },
        };

        match self.frame_context.unbind_surface() {
            Ok(Some(surface)) => Ok(ImportedSurfmanSurface {
                imported_texture,
                surface,
            }),
            Ok(None) => Err(SurfmanSurfaceImportFailure {
                error: SurfmanSurfaceImportError::MissingSurfaceAfterImport,
                surface: None,
            }),
            Err(error) => Err(SurfmanSurfaceImportFailure {
                error: SurfmanSurfaceImportError::UnbindSurface(error),
                surface: None,
            }),
        }
    }

    pub fn import_surface_default(
        &self,
        surface: Surface,
    ) -> Result<ImportedSurfmanSurface, SurfmanSurfaceImportFailure> {
        self.import_surface(surface, &ImportOptions::default())
    }

    pub fn importer(&self) -> &WgpuTextureImporter {
        &self.importer
    }
}

pub struct ServoWgpuRenderingContext {
    frame_producer: RefCell<SurfmanFrameProducer>,
    swap_chain: SwapChain<Device>,
}

impl Drop for ServoWgpuRenderingContext {
    fn drop(&mut self) {
        let surfman_rendering_info = self.frame_producer.borrow().context();
        let device = &mut surfman_rendering_info.device.borrow_mut();
        let context = &mut surfman_rendering_info.context.borrow_mut();
        let _ = self.swap_chain.destroy(device, context);
    }
}

impl ServoWgpuRenderingContext {
    pub fn new(size: PhysicalSize<u32>) -> Result<Self, surfman::Error> {
        let connection = Connection::new()?;
        let adapter = connection.create_adapter()?;
        let surfman_rendering_info = Rc::new(SurfmanFrameContext::new(&connection, &adapter)?);

        let surfman_size = Size2D::new(size.width as i32, size.height as i32);
        let surface =
            surfman_rendering_info.create_surface(SurfaceType::Generic { size: surfman_size })?;

        surfman_rendering_info.bind_surface(surface)?;
        surfman_rendering_info.make_current()?;

        let swap_chain = surfman_rendering_info.create_attached_swap_chain()?;

        Ok(Self {
            frame_producer: RefCell::new(SurfmanFrameProducer::new(surfman_rendering_info, size)),
            swap_chain,
        })
    }

    pub fn acquire_native_frame(
        &self,
    ) -> Result<wgpu_native_texture_interop::NativeFrame, InteropError> {
        self.frame_producer.borrow_mut().acquire_frame()
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.frame_producer.borrow().size()
    }

    /// Resize the surfman rendering context (e.g. after a window resize).
    pub fn resize_viewport(&self, size: PhysicalSize<u32>) {
        let surfman_rendering_info = self.frame_producer.borrow().context();
        if self.frame_producer.borrow().size() == size {
            return;
        }
        self.frame_producer.borrow().set_size(size);
        let mut device = surfman_rendering_info.device.borrow_mut();
        let mut context = surfman_rendering_info.context.borrow_mut();
        let size = euclid::default::Size2D::new(size.width as i32, size.height as i32);
        let _ = self.swap_chain.resize(&mut *device, &mut *context, size);
    }

    /// Read the full current frame as a CPU-side RGBA image.
    ///
    /// Returns `None` if the frame is not available (e.g. no surface bound yet).
    pub fn read_full_frame(&self) -> Option<image::RgbaImage> {
        let size = self.size();
        self.frame_producer.borrow().context().read_to_image_region(
            0,
            0,
            size.width as i32,
            size.height as i32,
        )
    }
}

#[cfg(feature = "servo")]
impl RenderingContext for ServoWgpuRenderingContext {
    fn prepare_for_rendering(&self) {
        self.frame_producer
            .borrow()
            .context()
            .prepare_for_rendering();
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<image::RgbaImage> {
        self.frame_producer.borrow().context().read_to_image_region(
            source_rectangle.min.x,
            source_rectangle.min.y,
            source_rectangle.width(),
            source_rectangle.height(),
        )
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.frame_producer.borrow().size()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        let surfman_rendering_info = self.frame_producer.borrow().context();
        if self.frame_producer.borrow().size() == size {
            return;
        }

        self.frame_producer.borrow().set_size(size);

        let mut device = surfman_rendering_info.device.borrow_mut();
        let mut context = surfman_rendering_info.context.borrow_mut();
        let size = Size2D::new(size.width as i32, size.height as i32);
        let _ = self.swap_chain.resize(&mut *device, &mut *context, size);
    }

    fn present(&self) {
        let surfman_rendering_info = self.frame_producer.borrow().context();
        let mut device = surfman_rendering_info.device.borrow_mut();
        let mut context = surfman_rendering_info.context.borrow_mut();
        let _ = self
            .swap_chain
            .swap_buffers(&mut *device, &mut *context, PreserveBuffer::No);
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.frame_producer.borrow().context().make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.frame_producer.borrow().context().gleam_gl.clone()
    }

    fn glow_gl_api(&self) -> std::sync::Arc<glow::Context> {
        self.frame_producer.borrow().context().glow_gl.clone()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        self.frame_producer
            .borrow()
            .context()
            .create_texture(surface)
    }

    fn destroy_texture(&self, surface_texture: SurfaceTexture) -> Option<Surface> {
        self.frame_producer
            .borrow()
            .context()
            .destroy_texture(surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        self.frame_producer.borrow().context().connection()
    }
}

/// A [`RenderingContext`] wrapper that captures a CPU copy of each frame
/// immediately before the swap-chain `present()` flip — the only point where
/// the rendered back-buffer is still bound to the GL context.
///
/// Call [`CapturingRenderingContext::take_frame`] after [`servo::WebView::paint`]
/// to obtain the most-recently rendered frame as an [`image::RgbaImage`].
#[cfg(feature = "servo")]
pub struct CapturingRenderingContext {
    inner: Rc<ServoWgpuRenderingContext>,
    last_frame: RefCell<Option<image::RgbaImage>>,
}

#[cfg(feature = "servo")]
impl CapturingRenderingContext {
    pub fn new(inner: Rc<ServoWgpuRenderingContext>) -> Self {
        Self {
            inner,
            last_frame: RefCell::new(None),
        }
    }

    /// Returns the last frame captured during `present()`, replacing the stored
    /// value with `None` so repeated calls without a new paint return `None`.
    pub fn take_frame(&self) -> Option<image::RgbaImage> {
        self.last_frame.borrow_mut().take()
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.inner.size()
    }

    pub fn resize(&self, size: PhysicalSize<u32>) {
        self.inner.resize(size);
    }
}

#[cfg(feature = "servo")]
impl RenderingContext for CapturingRenderingContext {
    fn prepare_for_rendering(&self) {
        self.inner.prepare_for_rendering();
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<image::RgbaImage> {
        self.inner.read_to_image(source_rectangle)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.inner.size()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.inner.resize(size);
    }

    /// Captures the rendered back-buffer **before** handing off to the swap chain.
    fn present(&self) {
        // Drain GL errors left by Servo's rendering — read_framebuffer_to_image
        // checks get_error() and returns None on *any* pending error, even ones
        // from the producer, not just from read_pixels.
        let gl = self.inner.gleam_gl_api();
        while gl.get_error() != gleam::gl::NO_ERROR {}

        // Read while the rendered frame is still bound to the context.
        let frame = self.inner.read_full_frame();
        *self.last_frame.borrow_mut() = frame;
        self.inner.present();
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.inner.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.inner.gleam_gl_api()
    }

    fn glow_gl_api(&self) -> std::sync::Arc<glow::Context> {
        self.inner.glow_gl_api()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        self.inner.create_texture(surface)
    }

    fn destroy_texture(&self, surface_texture: SurfaceTexture) -> Option<Surface> {
        self.inner.destroy_texture(surface_texture)
    }

    fn connection(&self) -> Option<Connection> {
        self.inner.connection()
    }
}

pub struct ServoWgpuInteropAdapter {
    importer: WgpuTextureImporter,
    rendering_context: Rc<ServoWgpuRenderingContext>,
}

impl ServoWgpuInteropAdapter {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        size: PhysicalSize<u32>,
    ) -> Result<Self, surfman::Error> {
        let rendering_context = Rc::new(ServoWgpuRenderingContext::new(size)?);
        let importer = WgpuTextureImporter::new(HostWgpuContext::new(device, queue));

        Ok(Self {
            importer,
            rendering_context,
        })
    }

    #[cfg(feature = "servo")]
    pub fn rendering_context(&self) -> Rc<dyn RenderingContext> {
        self.rendering_context.clone()
    }

    pub fn rendering_context_handle(&self) -> Rc<ServoWgpuRenderingContext> {
        self.rendering_context.clone()
    }

    pub fn import_current_frame(
        &self,
        options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError> {
        let frame = self.rendering_context.acquire_native_frame()?;
        self.importer.import_frame(&frame, options)
    }

    pub fn import_current_frame_default(&self) -> Result<ImportedTexture, InteropError> {
        self.import_current_frame(&ImportOptions::default())
    }

    pub fn importer(&self) -> &WgpuTextureImporter {
        &self.importer
    }
}
