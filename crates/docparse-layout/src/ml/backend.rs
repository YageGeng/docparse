//! Runtime backend selection.
//!
//! Exposes the WGPU Burn backend for layout inference.

use burn_wgpu::{Wgpu, WgpuDevice};

/// Selected GPU backend for layout inference.
pub(crate) type AutoBackend = Wgpu;

/// Device for [`AutoBackend`].
pub(crate) type AutoDevice = WgpuDevice;

/// Returns the selected backend's default device.
pub(crate) fn auto_device() -> AutoDevice {
    AutoDevice::default()
}

/// Initializes browser WebGPU for the default Burn device.
#[cfg(target_arch = "wasm32")]
pub async fn init_browser_webgpu() {
    let device = auto_device();
    burn_wgpu::init_setup_async::<burn_wgpu::graphics::WebGpu>(
        &device,
        burn_wgpu::RuntimeOptions::default(),
    )
    .await;
}
