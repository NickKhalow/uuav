use anyhow::{Result, anyhow};
use std::os::raw::c_void;

use crate::hw_device::HwDevice;
use crate::playback_unit::{PlaybackUnit, fill_silence};
use crate::{AudioOutConfig, ErrorCallback, UUAVState, VideoSize};

/// Engine-facing player: manages url switching by uploading a fresh
/// [`PlaybackUnit`] per opened url and delegates everything else to it.
pub(crate) struct UUAVPlayer {
    device: HwDevice,
    audio_out: AudioOutConfig,
    error_callback: ErrorCallback,
    playback: Option<PlaybackUnit>,
}

impl UUAVPlayer {
    pub(crate) fn new(
        device: HwDevice,
        audio_out: AudioOutConfig,
        error_callback: ErrorCallback,
    ) -> Self {
        Self {
            device,
            audio_out,
            error_callback,
            playback: None,
        }
    }

    // not blocking
    // async, returns immediately
    pub(crate) fn open_media_intent(&mut self, url: String) -> Result<()> {
        // dropping the previous unit implicitly closes its media
        self.playback = None;
        self.playback = Some(PlaybackUnit::new(
            url,
            self.device.clone(),
            self.audio_out.clone(),
            self.error_callback,
        )?);
        Ok(())
    }

    /// Never fails: dropping the unit stops the worker and releases every
    /// per-playback resource.
    pub(crate) fn close_media(&mut self) {
        self.playback = None;
    }

    fn playback_unit(&self) -> Result<&PlaybackUnit> {
        self.playback
            .as_ref()
            .ok_or_else(|| anyhow!("no media open"))
    }

    pub(crate) fn play(&self) -> Result<()> {
        self.playback_unit()?.play()
    }

    pub(crate) fn pause(&self) -> Result<()> {
        self.playback_unit()?.pause()
    }

    // not blocking
    pub(crate) fn seek_intent(&self, time: f64) -> Result<()> {
        self.playback_unit()?.seek_intent(time)
    }

    pub(crate) fn state(&self) -> UUAVState {
        if let Some(p) = &self.playback {
            p.state()
        }
        else {
            UUAVState::UUAV_CLOSED
        }
    }

    pub(crate) fn duration(&self) -> Option<f64> {
        self.playback.as_ref()?.duration()
    }

    pub(crate) fn current_time(&self) -> Option<f64> {
        self.playback.as_ref()?.current_time()
    }

    // the player slaves its playback to the externally provided master clock
    pub(crate) fn assign_master_clock(&self, current_time: f64) -> Result<()> {
        self.playback_unit()?.assign_master_clock(current_time)
    }

    pub(crate) fn video_size(&self) -> Option<VideoSize> {
        self.playback.as_ref()?.video_size()
    }

    // NV12 texture on the engine's device; the engine creates its own plane
    // views over it. Valid from the first presented frame; recreated (and to
    // be re-queried) on resolution change.
    pub(crate) fn video_texture(&self) -> Option<*const c_void> {
        self.playback.as_ref()?.video_texture()
    }

    // [render thread] presents the frame that is due per the clock
    pub(crate) fn on_render_event(&self) {
        if let Some(playback) = &self.playback {
            playback.on_render_event();
        }
    }

    // [audio thread] fills interleaved FLT; pads silence on underrun,
    // never blocks; returns frames actually copied
    pub(crate) fn read_audio(&self, dst: *mut f32, nb_frames: i32) -> i32 {
        let Ok(frames) = usize::try_from(nb_frames) else {
            return 0;
        };
        match &self.playback {
            Some(playback) => playback.read_audio(dst, frames),
            None => {
                // no media: silence in the engine's configured layout
                fill_silence(dst, frames, self.audio_out.current().channels_usize());
                0
            }
        }
    }
}
