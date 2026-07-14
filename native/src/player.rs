use std::os::raw::c_void;

use crate::hw_device::HwDevice;
use crate::{UUAVState, VideoSize};

pub(crate) struct UUAVPlayer {
    device: HwDevice,
}

unsafe impl Send for UUAVPlayer {}
unsafe impl Sync for UUAVPlayer {}

impl Drop for UUAVPlayer {
    fn drop(&mut self) {
        //void        uuav_player_free(UUAVPlayer* p);            // [main] joins decoder thread; see shutdown note
        todo!()
    }
}

impl UUAVPlayer {
    pub(crate) fn new(device: HwDevice) -> Self {
        // TODO initialisation?
        todo!();

        Self { device }
    }

    // not blocking
    // async, returns immidieatly
    pub(crate) fn open_media_intent(&mut self, url: String) -> anyhow::Result<()> {
        todo!();
    }

    pub(crate) fn close_media(&mut self) -> anyhow::Result<()> {
        todo!();
    }

    pub(crate) fn play(&self) -> anyhow::Result<()> {
        todo!();
    }

    pub(crate) fn pause(&self) -> anyhow::Result<()> {
        todo!();
    }

    // not blocking
    pub(crate) fn seek_intent(&self, time: f64) -> anyhow::Result<()> {
        todo!()
    }

    pub(crate) fn state(&self) -> UUAVState {
        todo!();
    }

    pub(crate) fn duration(&self) -> anyhow::Result<Option<f64>> {
        todo!()
    }

    pub(crate) fn current_time(&self) -> anyhow::Result<Option<f64>> {
        todo!()
    }

    // the player slaves its playback to the externally provided master clock
    pub(crate) fn assign_master_clock(&self, current_time: f64) -> anyhow::Result<()> {
        todo!()
    }

    pub(crate) fn video_size() -> anyhow::Result<Option<VideoSize>> {
        todo!()
    }

    // TODO> Who notifies the consumer about recreation? What is the lifetime of the previous texture?
    // SRV: plane 0 = Y, 1 = UV; valid from READY; recreated on resolution change
    pub(crate) fn video_texture(&self, plane: i32) -> anyhow::Result<*const c_void> {
        todo!()
    }

    // fills interleaved FLT; pads silence on underrun,
    // never blocks; returns frames actually copied
    pub(crate) fn read_audio(&self, dst: *mut f32, nb_frames: i32) -> i32 {
        todo!()
    }
}
