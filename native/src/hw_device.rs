use std::os::raw::c_void;

// Is guaranteed to live the whole lifecycle of the app, static lifetime
// D3D or Metal device
#[derive(Clone, Copy)]
pub(crate) struct HwDevice {
    // is passed from the master flow and remains valid for the whole
    // lifetime of the app
    raw_device: *const c_void,
}

unsafe impl Send for HwDevice {}
unsafe impl Sync for HwDevice {}

impl HwDevice {
    pub(crate) fn from_raw(raw_device: *const c_void) -> Self {
        Self { raw_device }
    }
}
