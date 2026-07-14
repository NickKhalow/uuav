use anyhow::{Result, anyhow};
use ffmpeg_sys_next as ff;
use std::os::raw::c_void;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::core::Interface;

use crate::ffutil::av_err;

/// The engine-provided `ID3D11Device`, validated once at init. Every clone
/// holds its own COM reference, so the device outlives whatever the engine
/// does with its pointer afterwards.
#[derive(Clone)]
pub(crate) struct HwDevice {
    device: ID3D11Device,
}

// D3D11 devices are free-threaded
unsafe impl Send for HwDevice {}
unsafe impl Sync for HwDevice {}

impl HwDevice {
    /// # Safety
    /// `raw_device` must be null (rejected) or a live `ID3D11Device*`.
    pub(crate) unsafe fn from_raw(raw_device: *const c_void) -> Result<Self> {
        let raw = raw_device.cast_mut();
        let device = unsafe { ID3D11Device::from_raw_borrowed(&raw) }
            .ok_or_else(|| anyhow!("hw_device is not a valid ID3D11Device"))?
            .clone();
        Ok(Self { device })
    }

    pub(crate) const fn device(&self) -> &ID3D11Device {
        &self.device
    }
}

/// Mirror of `AVD3D11VADeviceContext` from `libavutil/hwcontext_d3d11va.h`
/// (stable public ABI; the header is not covered by the generated bindings).
/// COM pointers are kept as raw `c_void` to avoid coupling the layout to
/// `windows`-crate types.
#[repr(C)]
struct AVD3D11VADeviceContext {
    device: *mut c_void,         // ID3D11Device*
    device_context: *mut c_void, // ID3D11DeviceContext* (immediate)
    video_device: *mut c_void,   // ID3D11VideoDevice*
    video_context: *mut c_void,  // ID3D11VideoContext*
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
}

/// A D3D11VA `AVHWDeviceContext` bound to the externally provided
/// `ID3D11Device` (Unity's device), so decoded surfaces are directly
/// usable for GPU copies into textures sampled by Unity.
pub(crate) struct HwDeviceContext {
    buf: *mut ff::AVBufferRef,
}

// AVBufferRef refcounting is atomic; the D3D11 device is free-threaded and
// serialized through the ffmpeg-provided lock where required.
unsafe impl Send for HwDeviceContext {}
unsafe impl Sync for HwDeviceContext {}

impl HwDeviceContext {
    pub(crate) fn new(device: &HwDevice) -> Result<Self> {
        unsafe {
            let mut buf = ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if buf.is_null() {
                return Err(anyhow!("av_hwdevice_ctx_alloc(D3D11VA) failed"));
            }

            let dev_ctx = (*buf).data.cast::<ff::AVHWDeviceContext>();
            let hwctx = (*dev_ctx).hwctx.cast::<AVD3D11VADeviceContext>();

            // av_hwdevice_ctx frees one reference on the device when the
            // context is destroyed, so hand it an AddRef'd pointer
            (*hwctx).device = device.device().clone().into_raw();

            let ret = ff::av_hwdevice_ctx_init(buf);
            if ret < 0 {
                ff::av_buffer_unref(&mut buf);
                return Err(av_err("av_hwdevice_ctx_init(D3D11VA)", ret));
            }

            Ok(Self { buf })
        }
    }

    pub(crate) const fn as_buffer_ptr(&self) -> *mut ff::AVBufferRef {
        self.buf
    }

    fn hwctx(&self) -> *mut AVD3D11VADeviceContext {
        unsafe {
            let dev_ctx = (*self.buf).data.cast::<ff::AVHWDeviceContext>();
            (*dev_ctx).hwctx.cast::<AVD3D11VADeviceContext>()
        }
    }

    /// The immediate `ID3D11DeviceContext*` owned by the hw context.
    /// Only touch it while holding [`Self::lock`] — the decoder threads
    /// of FFmpeg use the same context under the same lock.
    pub(crate) fn immediate_context_raw(&self) -> *mut c_void {
        unsafe { (*self.hwctx()).device_context }
    }

    pub(crate) fn lock(&self) -> HwLockGuard<'_> {
        unsafe {
            let hwctx = self.hwctx();
            if let Some(lock) = (*hwctx).lock {
                lock((*hwctx).lock_ctx);
            }
        }
        HwLockGuard { ctx: self }
    }
}

impl Drop for HwDeviceContext {
    fn drop(&mut self) {
        unsafe { ff::av_buffer_unref(&mut self.buf) };
    }
}

pub(crate) struct HwLockGuard<'a> {
    ctx: &'a HwDeviceContext,
}

impl Drop for HwLockGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            let hwctx = self.ctx.hwctx();
            if let Some(unlock) = (*hwctx).unlock {
                unlock((*hwctx).lock_ctx);
            }
        }
    }
}
