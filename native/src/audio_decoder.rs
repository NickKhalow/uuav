use anyhow::{Context, Result};
use ffmpeg_sys_next as ff;
use std::mem;
use std::os::raw::c_int;
use std::ptr;

use crate::AudioOptions;
use crate::ffutil::{AVERROR_EAGAIN, Decoded, OwnedDecoder, OwnedFrame, Stream, av_err, check, q2d};

/// Interleaved f32 samples in the configured output format.
pub(crate) struct AudioFrame {
    pts: Option<f64>,
    samples: Vec<f32>,
}

impl AudioFrame {
    /// Presentation time in seconds of the first sample, `None` when the
    /// container provides no usable timestamp.
    pub(crate) const fn pts(&self) -> Option<f64> {
        self.pts
    }

    pub(crate) fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Description of the samples entering the resampler;
/// a change (rare, but legal mid-stream) triggers a rebuild.
struct SwrInput {
    format: c_int,
    rate: c_int,
    layout: ff::AVChannelLayout,
}

impl SwrInput {
    fn from_frame_options(frame: &OwnedFrame) -> Result<Self> {
        let mut input = SwrInput {
            format: frame.format(),
            rate: frame.sample_rate(),
            layout: unsafe { mem::zeroed() },
        };
        check("av_channel_layout_copy", unsafe {
            ff::av_channel_layout_copy(&mut input.layout, frame.ch_layout())
        })?;

        Ok(input)
    }
}

impl Drop for SwrInput {
    fn drop(&mut self) {
        unsafe { ff::av_channel_layout_uninit(&mut self.layout) };
    }
}

/// Resampler converting decoded frames to interleaved `f32` at the output
/// rate and channel count it was built for. Owns the `SwrContext` and the
/// input description; both are released on drop.
struct Resampler {
    swr: *mut ff::SwrContext, // TODO Looks like SwrContext should be a separate wrapped type and it
                              // should own its creation cycle. It such case the comment "constructed before the fallible calls so a failed init still frees whatever swr_alloc_set_opts2 allocated" will become structurally redundant
    input: SwrInput,
    output: AudioOptions,
}

impl Resampler {

    /// Builds a resampler converting from the frame's sample format to
    /// interleaved `f32` at `output`.
    fn new(frame: &OwnedFrame, output: AudioOptions) -> Result<Self> {
        // constructed before the fallible calls so a failed init still
        // frees whatever swr_alloc_set_opts2 allocated
        let mut resampler = Self {
            swr: ptr::null_mut(),
            input: SwrInput::from_frame_options(frame)?,
            output,
        };

        unsafe {
            let mut out_layout: ff::AVChannelLayout = mem::zeroed();
            ff::av_channel_layout_default(&mut out_layout, output.channels);

            let ret = ff::swr_alloc_set_opts2(
                &mut resampler.swr,
                &out_layout,
                ff::AVSampleFormat::AV_SAMPLE_FMT_FLT,
                output.sample_rate,
                frame.ch_layout(),
                mem::transmute::<c_int, ff::AVSampleFormat>(resampler.input.format),
                resampler.input.rate,
                0,
                ptr::null_mut(),
            );
            ff::av_channel_layout_uninit(&mut out_layout);
            check("swr_alloc_set_opts2", ret)?;
            check("swr_init", ff::swr_init(resampler.swr))?;
        }

        Ok(resampler)
    }

    /// Whether the frame still matches the input this resampler was built
    /// for.
    fn matches(&self, frame: &OwnedFrame) -> bool {
        self.input.format == frame.format()
            && self.input.rate == frame.sample_rate()
            && unsafe { ff::av_channel_layout_compare(&self.input.layout, frame.ch_layout()) } == 0
    }

    /// Converts the frame's samples into interleaved `f32` at the output
    /// rate.
    fn convert(&mut self, frame: &OwnedFrame) -> Result<Vec<f32>> {
        let in_rate = frame.sample_rate().max(1);
        let in_samples = frame.nb_samples();
        let delay = unsafe { ff::swr_get_delay(self.swr, i64::from(in_rate)) };
        // generous headroom over the exact rescale to absorb rounding
        let out_capacity = (((delay as f64 + f64::from(in_samples))
            * f64::from(self.output.sample_rate)
            / f64::from(in_rate))
        .ceil()
            + 32.0) as c_int;

        // AudioOptions is sanitized at the FFI boundary: always positive
        let channels = self.output.channels as usize;
        let mut samples = vec![
            0.0_f32;
            usize::try_from(out_capacity)
                .unwrap_or(0)
                .saturating_mul(channels)
        ];

        let out_planes = [samples.as_mut_ptr().cast::<u8>()];
        let converted = check("swr_convert", unsafe {
            ff::swr_convert(
                self.swr,
                out_planes.as_ptr(),
                out_capacity,
                frame.extended_data(),
                in_samples,
            )
        })?;

        samples.truncate(
            usize::try_from(converted)
                .unwrap_or(0)
                .saturating_mul(channels),
        );
        Ok(samples)
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        unsafe { ff::swr_free(&mut self.swr) };
    }
}

/// Audio decoder + resampler producing interleaved `f32` at the sample rate
/// and channel count of the engine's audio output.
pub(crate) struct AudioDecoder {
    ctx: OwnedDecoder,
    time_base: ff::AVRational,
    resampler: Option<Resampler>,
    audio_options: AudioOptions,
}

impl AudioDecoder {
    pub(crate) fn new(stream: Stream, audio_options: AudioOptions) -> Result<Self> {
        let codec = stream.find_decoder().context("audio stream")?;
        let mut ctx = OwnedDecoder::new(codec)?;

        unsafe {
            check(
                "avcodec_parameters_to_context(audio)",
                ff::avcodec_parameters_to_context(ctx.as_mut_ptr(), stream.codecpar()),
            )?;
            (*ctx.as_mut_ptr()).pkt_timebase = stream.time_base();
            check(
                "avcodec_open2(audio)",
                ff::avcodec_open2(ctx.as_mut_ptr(), codec, ptr::null_mut()),
            )?;
        }

        Ok(Self {
            ctx,
            time_base: stream.time_base(),
            resampler: None,
            audio_options,
        })
    }

    /// Applies new output options. Returns `true` when the format changed
    /// (callers should discard already-buffered samples).
    pub(crate) fn set_output(&mut self, options: AudioOptions) -> bool {
        if options == self.audio_options {
            return false;
        }
        self.audio_options = options;
        self.resampler = None;
        true
    }

    pub(crate) const fn audio_options(&self) -> AudioOptions {
        self.audio_options
    }

    /// Sends a packet; pass `null` to signal end of stream.
    pub(crate) fn send(&mut self, packet: *const ff::AVPacket) -> Result<()> {
        let ret = unsafe { ff::avcodec_send_packet(self.ctx.as_mut_ptr(), packet) };
        if ret < 0 && ret != ff::AVERROR_EOF {
            return Err(av_err("avcodec_send_packet(audio)", ret));
        }
        Ok(())
    }

    pub(crate) fn receive(&mut self) -> Result<Decoded<AudioFrame>> {
        let mut frame = OwnedFrame::new()?;
        let ret = unsafe { ff::avcodec_receive_frame(self.ctx.as_mut_ptr(), frame.as_mut_ptr()) };
        if ret == AVERROR_EAGAIN {
            return Ok(Decoded::Again);
        }
        if ret == ff::AVERROR_EOF {
            return Ok(Decoded::Eof);
        }
        check("avcodec_receive_frame(audio)", ret)?;

        // reuse the resampler while the input matches; rebuilding is rare,
        // but legal mid-stream
        let resampler = match self.resampler.take() {
            Some(resampler) if resampler.matches(&frame) => resampler,
            _ => Resampler::new(&frame, self.audio_options)?,
        };
        let samples = self.resampler.insert(resampler).convert(&frame)?;

        let ts = frame.best_effort_timestamp();
        let pts = if ts == ff::AV_NOPTS_VALUE {
            None
        } else {
            Some(ts as f64 * q2d(self.time_base))
        };

        Ok(Decoded::Frame(AudioFrame { pts, samples }))
    }

    pub(crate) fn flush(&mut self) {
        unsafe { ff::avcodec_flush_buffers(self.ctx.as_mut_ptr()) };
        // discard resampler history along with the decoder state
        self.resampler = None;
    }
}
