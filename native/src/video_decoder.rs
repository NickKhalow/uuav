use anyhow::{Result, anyhow};
use ffmpeg_sys_next as ff;
use std::os::raw::{c_int, c_void};
use std::ptr;

use crate::ffutil::{Decoded, OwnedFrame, av_err, check, q2d};
use crate::hw_device::HwDeviceContext;

/// A decoded D3D11VA frame. The underlying decoder surface stays alive
/// (and thus valid for GPU copies) for as long as `frame` is held.
pub(crate) struct VideoFrame {
    frame: OwnedFrame,
    /// Presentation time in seconds of the media timeline, `None` when the
    /// container provides no usable timestamp.
    pub(crate) pts: Option<f64>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl VideoFrame {
    /// `ID3D11Texture2D*` of the decoder surface pool.
    pub(crate) const fn texture_raw(&self) -> *mut c_void {
        unsafe { (*self.frame.as_ptr()).data[0].cast::<c_void>() }
    }

    /// Array slice index inside the decoder's texture array.
    pub(crate) fn subresource(&self) -> u32 {
        let index = unsafe { (*self.frame.as_ptr()).data[1] } as usize;
        u32::try_from(index).unwrap_or(0)
    }
}

/// D3D11VA hardware video decoder. Hardware decoding is mandatory: the
/// stream is rejected if the codec offers no D3D11 path or the surfaces
/// are not NV12.
pub(crate) struct VideoDecoder {
    ctx: *mut ff::AVCodecContext,
    time_base: ff::AVRational,
}

impl VideoDecoder {
    /// Extra surfaces the decoder allocates on top of its own needs; must
    /// cover every frame the player can hold queued at once.
    pub(crate) const EXTRA_HW_FRAMES: c_int = 8;

    pub(crate) fn new(stream: *mut ff::AVStream, hw: &HwDeviceContext) -> Result<Self> {
        unsafe {
            let codec = ff::avcodec_find_decoder((*(*stream).codecpar).codec_id);
            if codec.is_null() {
                return Err(anyhow!(
                    "no video decoder for codec id {:?}",
                    (*(*stream).codecpar).codec_id
                ));
            }

            let mut ctx = ff::avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return Err(anyhow!("avcodec_alloc_context3 failed"));
            }

            match Self::init(ctx, stream, hw, codec) {
                Ok(()) => Ok(Self {
                    ctx,
                    time_base: (*stream).time_base,
                }),
                Err(e) => {
                    ff::avcodec_free_context(&mut ctx);
                    Err(e)
                }
            }
        }
    }

    unsafe fn init(
        ctx: *mut ff::AVCodecContext,
        stream: *mut ff::AVStream,
        hw: &HwDeviceContext,
        codec: *const ff::AVCodec,
    ) -> Result<()> {
        unsafe {
            check(
                "avcodec_parameters_to_context(video)",
                ff::avcodec_parameters_to_context(ctx, (*stream).codecpar),
            )?;
            (*ctx).pkt_timebase = (*stream).time_base;
            (*ctx).hw_device_ctx = ff::av_buffer_ref(hw.as_buffer_ptr());
            if (*ctx).hw_device_ctx.is_null() {
                return Err(anyhow!("failed to reference the D3D11 device context"));
            }
            (*ctx).get_format = Some(require_d3d11_format);
            (*ctx).extra_hw_frames = Self::EXTRA_HW_FRAMES;

            check(
                "avcodec_open2(video)",
                ff::avcodec_open2(ctx, codec, ptr::null_mut()),
            )?;
            Ok(())
        }
    }

    /// Sends a packet; pass `null` to signal end of stream.
    pub(crate) fn send(&mut self, packet: *const ff::AVPacket) -> Result<()> {
        let ret = unsafe { ff::avcodec_send_packet(self.ctx, packet) };
        // EOF from send just means the drain was already signalled
        if ret < 0 && ret != ff::AVERROR_EOF {
            return Err(av_err("avcodec_send_packet(video)", ret));
        }
        Ok(())
    }

    pub(crate) fn receive(&mut self) -> Result<Decoded<VideoFrame>> {
        let mut frame = OwnedFrame::new()?;
        let ret = unsafe { ff::avcodec_receive_frame(self.ctx, frame.as_mut_ptr()) };
        if ret == ff::AVERROR(libc::EAGAIN) {
            return Ok(Decoded::Again);
        }
        if ret == ff::AVERROR_EOF {
            return Ok(Decoded::Eof);
        }
        check("avcodec_receive_frame(video)", ret)?;

        unsafe {
            let f = frame.as_ptr();

            if (*f).format != ff::AVPixelFormat::AV_PIX_FMT_D3D11 as c_int {
                return Err(anyhow!(
                    "decoder produced a non-D3D11 frame (format {}); hardware decoding is mandatory",
                    (*f).format
                ));
            }

            let frames_ctx = (*f).hw_frames_ctx;
            if frames_ctx.is_null() {
                return Err(anyhow!("D3D11 frame without a hw frames context"));
            }
            let sw_format = (*(*frames_ctx).data.cast::<ff::AVHWFramesContext>()).sw_format;
            if sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 {
                return Err(anyhow!(
                    "unsupported D3D11 surface format {:?}; only NV12 is supported",
                    sw_format
                ));
            }

            let ts = (*f).best_effort_timestamp;
            let pts = if ts == ff::AV_NOPTS_VALUE {
                None
            } else {
                Some(ts as f64 * q2d(self.time_base))
            };

            let width = u32::try_from((*f).width)
                .map_err(|_| anyhow!("decoder returned negative width {}", (*f).width))?;
            let height = u32::try_from((*f).height)
                .map_err(|_| anyhow!("decoder returned negative height {}", (*f).height))?;

            Ok(Decoded::Frame(VideoFrame {
                frame,
                pts,
                width,
                height,
            }))
        }
    }

    pub(crate) fn flush(&mut self) {
        unsafe { ff::avcodec_flush_buffers(self.ctx) };
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.ctx) };
    }
}

/// Rejects everything but the D3D11 hardware pixel format: returning NONE
/// makes `avcodec` fail decoding instead of silently falling back to
/// software frames.
unsafe extern "C" fn require_d3d11_format(
    _ctx: *mut ff::AVCodecContext,
    list: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    unsafe {
        let mut p = list;
        while !p.is_null() && *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == ff::AVPixelFormat::AV_PIX_FMT_D3D11 {
                return ff::AVPixelFormat::AV_PIX_FMT_D3D11;
            }
            p = p.add(1);
        }
    }
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}
