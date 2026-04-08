use thiserror::Error;

use crate::sync::SyncMechanism;

/// Why a particular interop path is not available.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnsupportedReason {
    /// The platform code path exists but has not been implemented yet.
    PlatformNotImplemented,
    /// No wgpu backend could be detected on the host device.
    HostBackendUnavailable,
    /// The frame type requires a different wgpu backend than the host is using
    /// (e.g. `VulkanExternalImage` on a Metal device).
    HostBackendMismatch,
    /// A native frame variant (e.g. `VulkanExternalImage`) is defined in the
    /// API but the corresponding import logic is not yet implemented.
    NativeImportNotYetImplemented,
}

/// Errors that can occur during frame import or synchronization.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InteropError {
    /// The requested interop path is not supported for the given reason.
    #[error("unsupported interop path: {0:?}")]
    Unsupported(UnsupportedReason),

    /// The wgpu backend on the host device does not match what the frame
    /// source requires.
    #[error("backend mismatch: expected {expected}, found {actual}")]
    BackendMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    /// The frame data is invalid (e.g. a zero-valued FBO id).
    #[error("invalid frame: {0}")]
    InvalidFrame(&'static str),

    /// The synchronizer received a [`SyncMechanism`] it does not handle.
    #[error("unsupported synchronization mechanism: {0:?}")]
    UnsupportedSynchronization(SyncMechanism),

    /// A surfman-level error occurred while preparing the frame.
    #[error("surfman interop failed: {0}")]
    Surfman(String),

    /// A Vulkan API call failed during import.
    #[error("vulkan interop failed: {0}")]
    Vulkan(String),

    /// A Metal API call failed during import.
    #[error("metal interop failed: {0}")]
    Metal(String),

    /// A GL API call failed during import.
    #[error("OpenGL interop failed: {0}")]
    OpenGl(String),

    /// A D3D12 or DXGI API call failed during import.
    #[error("D3D12 interop failed: {0}")]
    Dx12(String),

    /// An ANGLE EGL API call failed during D3D11 share-handle import.
    #[error("ANGLE EGL interop failed: {0}")]
    Angle(String),
}
