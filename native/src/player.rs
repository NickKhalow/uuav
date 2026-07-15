use anyhow::{Result, anyhow};
use arc_swap::ArcSwap;
use std::os::raw::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

use crate::hw_device::HwDevice;
use crate::playback::{CancelToken, ReadOnlyCancelToken, PlaybackUnit, fill_silence, report};
use crate::{AudioOutConfig, ErrorCallback, UUAVState, VideoSize};

static PLAYBACK_INDEX: AtomicU64 = AtomicU64::new(1);

/// Lifecycle of the player's playback, shared with the playback thread.
enum Playback {
    /// Nothing: no media, no thread.
    Closed,
    /// The playback thread exists but has not opened the media yet.
    Opening,
    /// The open failed; the error already went through the callback.
    Failed,
    /// The media is open; the playback thread runs this unit.
    Active(Arc<PlaybackUnit>),
}

/// The playback thread of the currently open media and the token that
/// stops it.
struct PlaybackThread {
    cancel: CancelToken,
    handle: JoinHandle<()>,
}

impl PlaybackThread {
    fn stop(self) {
        self.cancel.cancel();
        // errors were already reported through the callback
        let _ = self.handle.join();
    }
}

/// Engine-facing player: manages url switching by spawning a fresh
/// playback thread per opened url and delegates everything else to the
/// [`PlaybackUnit`] that thread publishes.
pub(crate) struct UUAVPlayer {
    device: HwDevice,
    audio_out: AudioOutConfig,
    error_callback: ErrorCallback,
    playback: Arc<ArcSwap<Playback>>,
    thread: Option<PlaybackThread>,
}

impl Drop for UUAVPlayer {
    fn drop(&mut self) {
        self.close_media();
    }
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
            playback: Arc::new(ArcSwap::from_pointee(Playback::Closed)),
            thread: None,
        }
    }

    // not blocking
    // async, returns immediately
    pub(crate) fn open_media_intent(&mut self, url: String) -> Result<()> {
        self.close_media();

        let cancel = CancelToken::new();
        self.playback.store(Arc::new(Playback::Opening));

        let index = PLAYBACK_INDEX.fetch_add(1, Ordering::Relaxed);
        let spawned = thread::Builder::new()
            .name(format!("uuav-player-{index}"))
            .spawn({
                let cancel: ReadOnlyCancelToken = cancel.clone().into();
                let playback = Arc::clone(&self.playback);
                let device = self.device.clone();
                let audio_out = self.audio_out.clone();
                let error_callback = self.error_callback;
                move || {
                    match PlaybackUnit::open(
                        url,
                        device,
                        audio_out,
                        cancel.clone(),
                        error_callback,
                    ) {
                        Ok((unit, pipeline)) => {
                            let unit = Arc::new(unit);
                            playback.store(Arc::new(Playback::Active(Arc::clone(&unit))));
                            unit.run_blocking(pipeline);
                        }
                        Err(e) => {
                            // a cancelled open is a close, not a failure
                            if !cancel.is_cancelled() {
                                report(error_callback, &e.to_string());
                                playback.store(Arc::new(Playback::Failed));
                            }
                        }
                    }
                }
            });

        match spawned {
            Ok(handle) => {
                self.thread = Some(PlaybackThread { cancel, handle });
                Ok(())
            }
            Err(e) => {
                self.playback.store(Arc::new(Playback::Closed));
                Err(anyhow!("failed to spawn playback thread: {e}"))
            }
        }
    }

    /// Never fails: stopping the playback thread releases every
    /// per-playback resource.
    pub(crate) fn close_media(&mut self) {
        if let Some(thread) = self.thread.take() {
            thread.stop();
        }
        self.playback.store(Arc::new(Playback::Closed));
    }

    fn unit(&self) -> Option<Arc<PlaybackUnit>> {
        match self.playback.load().as_ref() {
            Playback::Active(unit) => Some(Arc::clone(unit)),
            Playback::Closed | Playback::Opening | Playback::Failed => None,
        }
    }

    fn active_unit(&self) -> Result<Arc<PlaybackUnit>> {
        self.unit()
            .ok_or_else(|| anyhow!("no media open: {:?}", self.state()))
    }

    pub(crate) fn play(&self) -> Result<()> {
        self.active_unit()?.play()
    }

    pub(crate) fn pause(&self) -> Result<()> {
        self.active_unit()?.pause()
    }

    // not blocking
    pub(crate) fn seek_intent(&self, time: f64) -> Result<()> {
        self.active_unit()?.seek_intent(time)
    }

    pub(crate) fn state(&self) -> UUAVState {
        match self.playback.load().as_ref() {
            Playback::Closed => UUAVState::UUAV_CLOSED,
            Playback::Opening => UUAVState::UUAV_OPENING,
            Playback::Failed => UUAVState::UUAV_ERROR,
            Playback::Active(unit) => unit.state(),
        }
    }

    pub(crate) fn duration(&self) -> Option<f64> {
        self.unit()?.duration()
    }

    pub(crate) fn current_time(&self) -> Option<f64> {
        self.unit()?.current_time()
    }

    // the player slaves its playback to the externally provided master clock
    pub(crate) fn assign_master_clock(&self, current_time: f64) -> Result<()> {
        self.active_unit()?.assign_master_clock(current_time)
    }

    pub(crate) fn video_size(&self) -> Option<VideoSize> {
        self.unit()?.video_size()
    }

    // NV12 texture on the engine's device; the engine creates its own plane
    // views over it. Valid from the first presented frame; recreated (and to
    // be re-queried) on resolution change.
    pub(crate) fn video_texture(&self) -> Option<*const c_void> {
        self.unit()?.video_texture()
    }

    // [render thread] presents the frame that is due per the clock
    pub(crate) fn on_render_event(&self) {
        if let Some(unit) = self.unit() {
            unit.on_render_event();
        }
    }

    // [audio thread] fills interleaved FLT; pads silence on underrun,
    // never blocks; returns frames actually copied
    pub(crate) fn read_audio(&self, dst: *mut f32, nb_frames: i32) -> i32 {
        let Ok(frames) = usize::try_from(nb_frames) else {
            return 0;
        };
        match self.unit() {
            Some(unit) => unit.read_audio(dst, frames),
            None => {
                // no media (or still opening): silence in the engine's layout
                fill_silence(dst, frames, self.audio_out.current().channels_usize());
                0
            }
        }
    }
}
