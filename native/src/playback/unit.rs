use anyhow::{Result, anyhow};
use ffmpeg_sys_next as ff;
use parking_lot::Mutex;
use std::os::raw::c_void;
use std::sync::Once;
use std::thread;

use super::audio_playback::{AudioPlayback, AudioReader};
use super::input::Input;
use super::report;
use super::transport::{AtomicTransport, PlaybackState};
use super::util::{AtomicSeekSlot, PLAYBACK_POLL, ReadOnlyCancelToken};
use super::video_playback::{VideoPlayback, VideoQueue, VideoReader};
use crate::ffutil::{OwnedPacket, av_err, check};
use crate::hw_device::{HwDevice, HwDeviceContext};
use crate::video_output::VideoOutput;
use crate::{AudioOptionsView, ErrorCallback, UUAVState, VideoSize};

static NETWORK_INIT: Once = Once::new();

/// A video frame is presented once the clock is within this of its pts.
const VIDEO_PRESENT_BIAS: f64 = 0.005;

/// The demux/decode half of a playback: everything [`PlaybackUnit::run_blocking`]
/// needs mutable and exclusive on the playback thread.
pub(crate) struct Pipeline {
    input: Input,
    video: Option<VideoPlayback>,
    audio: Option<AudioPlayback>,
    /// Timestamp of the first frame; the media timeline is normalized so
    /// playback starts at 0.
    start_offset: f64,
}

/// Playback of a single url: the shared state the engine-facing threads
/// (control, render, audio) and the playback thread work against.
///
/// A unit that exists is fully open — [`PlaybackUnit::open`] builds it
/// from an already-opened media, so every field here is plain data.
pub(crate) struct PlaybackUnit {
    url: String,
    cancel: ReadOnlyCancelToken,
    /// Coalescing seek command; the playback thread services it.
    seek: AtomicSeekSlot,
    /// Playback state and media clock, as one atomic snapshot.
    transport: AtomicTransport,
    /// Audio-thread half of the playback's audio; the pipeline's
    /// [`AudioPlayback`] feeds it.
    audio: AudioReader,
    /// Render-thread half of the presentation queue; the pipeline's
    /// [`VideoPlayback`] feeds it.
    video: VideoReader,
    /// D3D11VA device context frames decode through; `None` for
    /// audio-only media.
    hw_ctx: Option<HwDeviceContext>,
    /// Total duration in seconds, `None` for realtime streams.
    duration: Option<f64>,
    video_size: Option<VideoSize>,
    /// Presentation target; created lazily on the render thread.
    output: Mutex<Option<VideoOutput>>,
    /// Engine device the presentation output is created on.
    device: HwDevice,
    error_callback: ErrorCallback,
}

impl PlaybackUnit {
    /// [playback thread] Opens the media at `url`, blocking until its
    /// streams are probed and the decoders are built. Aborted through
    /// `cancel` while blocked on demuxer I/O.
    pub(crate) fn open(
        url: String,
        device: HwDevice,
        audio_out: AudioOptionsView,
        cancel: ReadOnlyCancelToken,
        error_callback: ErrorCallback,
    ) -> Result<(Self, Pipeline)> {
        NETWORK_INIT.call_once(|| {
            unsafe { ff::avformat_network_init() };
        });

        let input = Input::open(cancel.clone(), &url)?;
        let video_index = input.find_best_stream(ff::AVMediaType::AVMEDIA_TYPE_VIDEO);
        let audio_index = input.find_best_stream(ff::AVMediaType::AVMEDIA_TYPE_AUDIO);
        if video_index < 0 && audio_index < 0 {
            return Err(anyhow!("media has no playable video or audio stream"));
        }

        let audio_reader = AudioReader::new(audio_out.clone());
        let (video_queue, video_reader) = VideoQueue::channel();

        let (hw_ctx, video, video_size) = if video_index >= 0 {
            let hw = HwDeviceContext::new(&device)?;
            let playback =
                VideoPlayback::new(input.stream_at(video_index), video_index, &hw, video_queue)?;
            let par = input.stream_at(video_index).codecpar();
            let (width, height) = unsafe { ((*par).width, (*par).height) };
            let size = VideoSize {
                width: u32::try_from(width).map_err(|_| anyhow!("invalid video width: {width}"))?,
                height: u32::try_from(height)
                    .map_err(|_| anyhow!("invalid video height: {height}"))?,
            };
            (Some(hw), Some(playback), Some(size))
        } else {
            (None, None, None)
        };

        let audio = if audio_index >= 0 {
            Some(AudioPlayback::new(
                input.stream_at(audio_index),
                audio_index,
                audio_out,
                audio_reader.rx_slot(),
            )?)
        } else {
            None
        };

        let unit = Self {
            duration: input.duration(),
            video_size,
            cancel,
            seek: AtomicSeekSlot::new(),
            transport: AtomicTransport::new(),
            audio: audio_reader,
            video: video_reader,
            hw_ctx,
            output: Mutex::new(None),
            device,
            error_callback,
            url,
        };
        let pipeline = Pipeline {
            start_offset: input.start_offset(),
            input,
            video,
            audio,
        };
        Ok((unit, pipeline))
    }

    /// [playback thread] Demuxes and decodes until the player cancels.
    /// A cancelled run is a clean exit; a runtime error propagates to the
    /// player, which retires this unit.
    pub(crate) fn run_blocking(&self, pipeline: Pipeline) -> Result<()> {
        match self.run(pipeline) {
            Err(e) if !self.cancel.is_cancelled() => Err(e),
            Err(_) | Ok(()) => Ok(()),
        }
    }

    fn run(&self, pipeline: Pipeline) -> Result<()> {
        let Pipeline {
            input,
            mut video,
            mut audio,
            start_offset,
        } = pipeline;

        let mut packet = OwnedPacket::new()?;
        // whether the demuxer ran out of input; playback then drains the sinks
        let mut eof = false;

        loop {
            if self.cancel.is_cancelled() {
                return Ok(());
            }

            if let Some(target) = self.seek.take() {
                // non-fatal: an unseekable (e.g. realtime) stream keeps playing
                if let Err(e) = self.apply_seek(
                    &input,
                    video.as_mut(),
                    audio.as_mut(),
                    &mut eof,
                    target,
                    start_offset,
                ) {
                    report(
                        self.error_callback,
                        &format!("seek to {target}s failed: {e}"),
                    );
                }
                continue;
            }

            if eof {
                self.settle_ended(video.as_ref(), audio.as_ref());
                thread::sleep(PLAYBACK_POLL);
                continue;
            }

            let ret = unsafe { ff::av_read_frame(input.as_ptr(), packet.as_mut_ptr()) };
            if ret == ff::AVERROR_EOF {
                if let Some(video) = video.as_mut() {
                    video.drain(start_offset, &self.cancel, &self.seek)?;
                }
                if let Some(audio) = audio.as_mut() {
                    audio.drain(start_offset, &self.cancel, &self.seek)?;
                }
                eof = true;
                continue;
            }
            if ret == ff::AVERROR(libc::EAGAIN) {
                thread::sleep(PLAYBACK_POLL);
                continue;
            }
            if ret < 0 {
                return Err(av_err("av_read_frame", ret));
            }

            let stream_index = packet.stream_index();
            if let Some(video) = video.as_mut()
                && video.handles(stream_index)
            {
                video.handle_packet(&mut packet, start_offset, &self.cancel, &self.seek)?;
            } else if let Some(audio) = audio.as_mut()
                && audio.handles(stream_index)
            {
                audio.handle_packet(&mut packet, start_offset, &self.cancel, &self.seek)?;
            }
            packet.unref();
        }
    }

    /// Fully decoded; once the sinks have drained, holds the clock and
    /// flags ENDED.
    fn settle_ended(&self, video: Option<&VideoPlayback>, audio: Option<&AudioPlayback>) {
        let drained = video.is_none_or(VideoPlayback::is_drained)
            && audio.is_none_or(AudioPlayback::is_drained);
        if drained && self.transport.is_playing() {
            self.transport.ended();
        }
    }

    fn apply_seek(
        &self,
        input: &Input,
        video: Option<&mut VideoPlayback>,
        audio: Option<&mut AudioPlayback>,
        eof: &mut bool,
        target: f64,
        start_offset: f64,
    ) -> Result<()> {
        let ts = ((target + start_offset) * f64::from(ff::AV_TIME_BASE)) as i64;
        check("avformat_seek_file", unsafe {
            ff::avformat_seek_file(input.as_ptr(), -1, i64::MIN, ts, i64::MAX, 0)
        })?;

        if let Some(video) = video {
            video.flush_for_seek();
        }
        if let Some(audio) = audio {
            audio.flush_for_seek();
        }
        self.transport.rebase(target);
        *eof = false;
        Ok(())
    }

    pub(crate) fn state(&self) -> UUAVState {
        self.transport.state().into()
    }

    pub(crate) fn play(&self) {
        match self.transport.state() {
            PlaybackState::Ready | PlaybackState::Paused => {}
            PlaybackState::Playing => return,
            PlaybackState::Ended => {
                // restart from the beginning
                self.seek.request(0.0);
            }
        }
        self.transport.play();
    }

    pub(crate) fn pause(&self) {
        match self.transport.state() {
            PlaybackState::Playing => self.transport.pause(),
            PlaybackState::Ready | PlaybackState::Paused | PlaybackState::Ended => {}
        }
    }

    pub(crate) fn seek_intent(&self, time: f64) {
        self.seek.request(time.max(0.0));
    }

    pub(crate) const fn duration(&self) -> Option<f64> {
        self.duration
    }

    pub(crate) fn current_time(&self) -> f64 {
        let now = self.transport.now();
        self.duration.map_or(now, |d| now.clamp(0.0, d))
    }

    /// The playback slaves itself to the externally provided master clock.
    pub(crate) fn assign_master_clock(&self, current_time: f64) {
        self.transport.sync_to_master(current_time);
    }

    pub(crate) fn video_size(&self) -> Option<VideoSize> {
        self.video_size.clone()
    }

    pub(crate) fn video_texture(&self) -> Option<*const c_void> {
        self.output
            .lock()
            .as_ref()
            .and_then(VideoOutput::texture_raw)
            .map(<*mut c_void>::cast_const)
    }

    /// [render thread] Presents the frame that is due per the clock.
    pub(crate) fn on_render_event(&self) {
        let now = self.transport.now() + VIDEO_PRESENT_BIAS;

        let Some(hw) = self.hw_ctx.as_ref() else {
            return;
        };
        let Some(frame) = self.video.next_due(now) else {
            return;
        };

        let mut output = self.output.lock();
        let out = output.get_or_insert_with(|| VideoOutput::new(&self.device));
        if let Err(e) = out.present(hw, &frame) {
            let message = format!("video present failed for {}: {e}", self.url);
            // Exit the guard scope before calling external FFI function
            drop(output);
            report(self.error_callback, &message);
        }
    }

    /// [audio thread] Fills interleaved FLT; pads silence on underrun,
    /// never blocks; returns frames actually copied.
    pub(crate) fn read_audio(&self, dst: *mut f32, frames: usize) -> i32 {
        self.audio.read(&self.transport, dst, frames)
    }
}
