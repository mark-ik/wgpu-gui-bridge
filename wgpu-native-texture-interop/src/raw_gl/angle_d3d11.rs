//! Zero-copy Servo frame import via ANGLE's D3D11 KMT shared texture handle.
//!
//! ## How it works
//!
//! Servo on Windows uses ANGLE, which implements OpenGL ES on top of D3D11.
//! Every ANGLE EGL pbuffer surface is backed by a real `ID3D11Texture2D`. ANGLE
//! exposes its handle through the `EGL_ANGLE_query_surface_pointer` extension:
//!
//! ```c
//! eglQuerySurfacePointerANGLE(display, surface,
//!     EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE, &share_handle);
//! ```
//!
//! ANGLE creates its D3D11 surface with `D3D11_RESOURCE_MISC_SHARED` (not
//! `SHARED_NTHANDLE`), so the returned handle is a KMT (legacy kernel-mode)
//! handle. Vulkan imports KMT handles via
//! `VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT` instead of
//! the NT-handle type used by `wgpu_hal::texture_from_d3d11_shared_handle`.
//! We implement the import directly against the raw ash Vulkan device.
//!
//! ## Preconditions
//!
//! - The ANGLE EGL context must be current on the calling thread (surfman's
//!   `make_context_current` guarantees this before we are called).
//! - The host wgpu device must use the Vulkan backend.
//! - The GPU driver must support `VK_KHR_external_memory_win32` (all modern
//!   NVIDIA, AMD, and Intel Vulkan drivers on Windows 10/11 do).
//!
//! ## Synchronization
//!
//! ANGLE's D3D11 backend uses `IDXGIKeyedMutex` to synchronize between D3D11
//! and EGL rendering. After Servo finishes a frame, ANGLE releases the mutex
//! (key=0). The Vulkan import does not acquire the keyed mutex itself; the
//! `consumer_sync` field is set to `ImplicitGlFlush` which causes the caller
//! to issue a `gl::flush()` before sampling the texture. This is sufficient
//! for the common single-GPU case where D3D11 and Vulkan share the same
//! device timeline.

use std::ffi::c_void;

use ash::vk;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use winit::dpi::PhysicalSize;

use crate::{HostWgpuContext, InteropError};

// ── EGL constants ────────────────────────────────────────────────────────────

const EGL_DRAW: i32 = 0x3059;
/// `EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE` — attribute passed to
/// `eglQuerySurfacePointerANGLE` to retrieve the D3D11 KMT shared handle.
const EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE: i32 = 0x3200;

// ── EGL function types ───────────────────────────────────────────────────────

type FnGetCurrentDisplay = unsafe extern "system" fn() -> *mut c_void;
type FnGetCurrentSurface = unsafe extern "system" fn(readdraw: i32) -> *mut c_void;
type FnQuerySurfacePointerANGLE = unsafe extern "system" fn(
    dpy: *mut c_void,
    surface: *mut c_void,
    attribute: i32,
    value: *mut *mut c_void,
) -> u32; // EGLBoolean

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Load a function from `libEGL.dll` by name using the Win32 module handle.
///
/// `libEGL.dll` is already loaded into the process by mozangle (Servo's ANGLE
/// build). `GetModuleHandleA` returns the handle without incrementing the
/// reference count; no `FreeLibrary` is needed.
unsafe fn get_egl_fn(name: &[u8]) -> Option<*const c_void> {
    // Safety: name must be a valid null-terminated ANSI string slice.
    let module = unsafe {
        GetModuleHandleA(windows::core::PCSTR(b"libEGL.dll\0".as_ptr())).ok()?
    };
    unsafe {
        GetProcAddress(module, windows::core::PCSTR(name.as_ptr()))
            .map(|f| f as *const c_void)
    }
}

/// Import a D3D11 texture into Vulkan using the KMT (legacy) handle type.
///
/// ANGLE surfaces use `D3D11_RESOURCE_MISC_SHARED` (not `SHARED_NTHANDLE`),
/// so `EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE` returns a KMT handle. We must
/// use `VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT` to import it.
///
/// # Safety
///
/// - `share_handle` must be a valid KMT shared handle from ANGLE's EGL pbuffer.
/// - `hal_device` must be the Vulkan HAL device backing the calling wgpu device.
unsafe fn import_kmt_texture(
    hal_device: &wgpu_hal::vulkan::Device,
    share_handle: HANDLE,
    desc: &wgpu_hal::TextureDescriptor,
) -> Result<wgpu_hal::vulkan::Texture, String> {
    let raw_device = hal_device.raw_device();
    let raw_physical = hal_device.raw_physical_device();
    let raw_instance = hal_device.shared_instance().raw_instance();

    // 1. Create VkImage with D3D11_TEXTURE_KMT external handle type.
    let mut ext_mem_info = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE_KMT);

    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: desc.size.width,
            height: desc.size.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut ext_mem_info);

    let vk_image = unsafe { raw_device.create_image(&image_info, None) }
        .map_err(|e| format!("vkCreateImage failed: {e:?}"))?;

    // 2. Memory requirements.
    let mem_req = unsafe { raw_device.get_image_memory_requirements(vk_image) };

    // 3. Find a DEVICE_LOCAL memory type compatible with this image.
    let mem_props =
        unsafe { raw_instance.get_physical_device_memory_properties(raw_physical) };

    let mem_type_index = (0..mem_props.memory_type_count as usize)
        .find(|&i| {
            let type_ok = mem_req.memory_type_bits & (1 << i) != 0;
            let flags_ok = mem_props.memory_types[i]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL);
            type_ok && flags_ok
        })
        .ok_or_else(|| "no DEVICE_LOCAL memory type found for KMT import".to_string())?;

    // 4. Allocate VkDeviceMemory importing the KMT handle.
    //    Chain: MemoryAllocateInfo → ImportMemoryWin32HandleInfoKHR → MemoryDedicatedAllocateInfo
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
    let mut import_info = vk::ImportMemoryWin32HandleInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE_KMT)
        .handle(share_handle.0 as _);
    // ash does not yet implement push_next for ImportMemoryWin32HandleInfoKHR
    // chaining to MemoryDedicatedAllocateInfo, so we wire p_next manually.
    import_info.p_next = std::ptr::from_mut(&mut dedicated).cast();

    let alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(mem_req.size)
        .memory_type_index(mem_type_index as u32)
        .push_next(&mut import_info);

    let memory = unsafe { raw_device.allocate_memory(&alloc_info, None) }
        .map_err(|e| format!("vkAllocateMemory (KMT import) failed: {e:?}"))?;

    // 5. Bind image to the imported memory.
    unsafe { raw_device.bind_image_memory(vk_image, memory, 0) }
        .map_err(|e| {
            unsafe { raw_device.free_memory(memory, None) };
            format!("vkBindImageMemory failed: {e:?}")
        })?;

    // 6. Wrap in a wgpu-hal Vulkan texture (wgpu-hal owns the VkDeviceMemory).
    Ok(unsafe {
        hal_device.texture_from_raw(
            vk_image,
            desc,
            None,
            wgpu_hal::vulkan::TextureMemory::Dedicated(memory),
        )
    })
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Query ANGLE for the D3D11 KMT shared handle backing the current EGL draw
/// surface.
///
/// Returns `None` if the current EGL context is not ANGLE, if the surface is
/// not a pbuffer, or if `libEGL.dll` is not loaded in the process.
pub fn query_angle_share_handle() -> Option<HANDLE> {
    // Load standard EGL entry points from the already-loaded libEGL.dll.
    let get_display: FnGetCurrentDisplay = unsafe {
        let ptr = get_egl_fn(b"eglGetCurrentDisplay\0")?;
        std::mem::transmute(ptr)
    };
    let get_surface: FnGetCurrentSurface = unsafe {
        let ptr = get_egl_fn(b"eglGetCurrentSurface\0")?;
        std::mem::transmute(ptr)
    };
    let query_ptr: FnQuerySurfacePointerANGLE = unsafe {
        let ptr = get_egl_fn(b"eglQuerySurfacePointerANGLE\0")?;
        std::mem::transmute(ptr)
    };

    let display = unsafe { get_display() };
    let surface = unsafe { get_surface(EGL_DRAW) };

    if display.is_null() || surface.is_null() {
        return None;
    }

    let mut raw_handle: *mut c_void = std::ptr::null_mut();
    let ok = unsafe {
        query_ptr(
            display,
            surface,
            EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE,
            &mut raw_handle,
        )
    };

    // EGL_FALSE == 0; also guard against a null/invalid handle value.
    if ok == 0 || raw_handle.is_null() {
        return None;
    }

    Some(HANDLE(raw_handle))
}

/// Import the D3D11 texture backing the current ANGLE EGL surface into a
/// `wgpu::Texture` via `VK_KHR_external_memory_win32` (KMT handle type).
///
/// # Errors
///
/// Returns [`InteropError::Angle`] if the share handle cannot be obtained.
///
/// Returns [`InteropError::BackendMismatch`] if `host` is not using the Vulkan
/// backend.
///
/// Returns [`InteropError::Vulkan`] if the Vulkan import fails.
pub fn import_angle_d3d11_frame(
    size: PhysicalSize<u32>,
    host: &HostWgpuContext,
) -> Result<wgpu::Texture, InteropError> {
    let share_handle = query_angle_share_handle().ok_or_else(|| {
        InteropError::Angle(
            "eglQuerySurfacePointerANGLE returned no handle — \
             is the current EGL context an ANGLE pbuffer surface?"
                .into(),
        )
    })?;

    let hal_device = unsafe { host.device.as_hal::<wgpu::wgc::api::Vulkan>() }
        .ok_or(InteropError::BackendMismatch {
            expected: "Vulkan",
            actual: "non-Vulkan (enable Backends::VULKAN)",
        })?;

    // ANGLE surfaces use D3D11_RESOURCE_MISC_SHARED (legacy/KMT handle), not
    // SHARED_NTHANDLE. We must import with D3D11_TEXTURE_KMT handle type.
    let hal_desc = wgpu_hal::TextureDescriptor {
        label: Some("angle-d3d11-kmt-import"),
        size: wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COLOR_TARGET,
        view_formats: vec![],
        memory_flags: wgpu_hal::MemoryFlags::empty(),
    };

    // SAFETY: share_handle is a valid KMT handle from ANGLE's EGL pbuffer;
    // hal_device is the Vulkan HAL device backing `host.device`.
    let hal_texture = unsafe { import_kmt_texture(&*hal_device, share_handle, &hal_desc) }
        .map_err(|e| InteropError::Vulkan(format!("D3D11 KMT handle import failed: {e}")))?;

    // SAFETY: hal_texture was created from host.device's underlying HAL device.
    let texture = unsafe {
        host.device
            .create_texture_from_hal::<wgpu::wgc::api::Vulkan>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("angle-d3d11-kmt-import"),
                    size: wgpu::Extent3d {
                        width: size.width,
                        height: size.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                },
            )
    };

    Ok(texture)
}
