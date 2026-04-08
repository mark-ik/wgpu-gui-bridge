use crate::{
    GlFramebufferSource, HostWgpuContext, ImportOptions, ImportedTexture, InteropError,
    SyncMechanism, TextureOrigin,
};

use super::SurfmanGlFrameSource;

pub(super) fn import_current_frame(
    source: &SurfmanGlFrameSource,
    frame: &GlFramebufferSource,
    host: &HostWgpuContext,
    _options: &ImportOptions,
) -> Result<ImportedTexture, InteropError> {
    let device = &source.context.device.borrow();
    let mut context = source.context.context.borrow_mut();

    // Make the context current WITH the surface still bound so that
    // eglGetCurrentSurface(EGL_DRAW) returns the ANGLE pbuffer.
    // Servo may have released the context after rendering; this restores it.
    device
        .make_context_current(&mut context)
        .map_err(|err| InteropError::Surfman(format!("{err:?}")))?;

    // ── Fast path: ANGLE D3D11 share handle (zero-copy, no GL extension needed) ──
    // The context is now current with the pbuffer as the draw surface, so
    // eglQuerySurfacePointerANGLE can retrieve the backing D3D11 handle.
    match crate::raw_gl::angle_d3d11::import_angle_d3d11_frame(source.size, host) {
        Ok(texture) => {
            return Ok(ImportedTexture {
                texture,
                format: wgpu::TextureFormat::Bgra8Unorm,
                size: frame.size(),
                origin: TextureOrigin::TopLeft,
                generation: source.generation,
                consumer_sync: SyncMechanism::ImplicitGlFlush,
            });
        }
        Err(_) => {
            // Not an ANGLE context, or Vulkan backend unavailable; try GL extension path.
        }
    }

    // ── Slow path: GL_EXT_memory_object_win32 (non-ANGLE Vulkan GL) ──────────────
    let surface = device
        .unbind_surface_from_context(&mut context)
        .map_err(|err| InteropError::Surfman(format!("{err:?}")))?
        .ok_or(InteropError::InvalidFrame("no surfman surface available"))?;

    device
        .make_context_current(&mut context)
        .map_err(|err| InteropError::Surfman(format!("{err:?}")))?;

    let surface_info = device.surface_info(&surface);
    let source_fbo = surface_info
        .framebuffer_object
        .map(|fb| fb.0.get())
        .unwrap_or(0);

    let result = crate::raw_gl::windows::import_gl_framebuffer_vulkan_win32(
        &source.context.glow_gl,
        &|name| device.get_proc_address(&context, name),
        source_fbo,
        source.size,
        host,
    );

    let _ = device
        .bind_surface_to_context(&mut context, surface)
        .map_err(|(err, mut surface)| {
            let _ = device.destroy_surface(&mut context, &mut surface);
            err
        });

    result.map(|texture| ImportedTexture {
        texture,
        format: wgpu::TextureFormat::Rgba8Unorm,
        size: frame.size(),
        origin: TextureOrigin::TopLeft,
        generation: source.generation,
        consumer_sync: SyncMechanism::ImplicitGlFlush,
    })
}

/// Import the current surfman frame into a `wgpu::Texture` via a D3D12 shared texture.
///
/// Use this when the host wgpu device uses the D3D12 backend.
pub(super) fn import_current_frame_dx12(
    source: &SurfmanGlFrameSource,
    frame: &GlFramebufferSource,
    host: &HostWgpuContext,
    _options: &ImportOptions,
) -> Result<ImportedTexture, InteropError> {
    let device = &source.context.device.borrow();
    let mut context = source.context.context.borrow_mut();

    let surface = device
        .unbind_surface_from_context(&mut context)
        .map_err(|err| InteropError::Surfman(format!("{err:?}")))?
        .ok_or(InteropError::InvalidFrame("no surfman surface available"))?;

    device
        .make_context_current(&mut context)
        .map_err(|err| InteropError::Surfman(format!("{err:?}")))?;

    let surface_info = device.surface_info(&surface);
    let source_fbo = surface_info
        .framebuffer_object
        .map(|fb| fb.0.get())
        .unwrap_or(0);

    let result = crate::raw_gl::dx12::import_gl_framebuffer_dx12(
        &source.context.glow_gl,
        &|name| device.get_proc_address(&context, name),
        source_fbo,
        source.size,
        host,
    );

    let _ = device
        .bind_surface_to_context(&mut context, surface)
        .map_err(|(err, mut surface)| {
            let _ = device.destroy_surface(&mut context, &mut surface);
            err
        });

    result.map(|texture| ImportedTexture {
        texture,
        format: wgpu::TextureFormat::Rgba8Unorm,
        size: frame.size(),
        origin: TextureOrigin::TopLeft,
        generation: source.generation,
        consumer_sync: SyncMechanism::ImplicitGlFlush,
    })
}
