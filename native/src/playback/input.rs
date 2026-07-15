use anyhow::{Result, anyhow};
use ffmpeg_sys_next as ff;
use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use super::util::ReadOnlyCancelToken;
use crate::ffutil::{Stream, check};

/// Aborts blocking demuxer I/O when the playback is being closed.
unsafe extern "C" fn interrupt_cb(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 0;
    }
    let cancelled = unsafe { &*opaque.cast::<AtomicBool>() };
    c_int::from(cancelled.load(Ordering::Relaxed))
}

/// Owning wrapper around the demuxer of one opened url.
pub(super) struct Input {
    fmt: *mut ff::AVFormatContext,
    /// Never read: keeps the flag the demuxer's interrupt callback points
    /// at alive for as long as the context holds the raw pointer.
    _cancel: ReadOnlyCancelToken,
}

impl Input {
    /// Opens the media and probes its streams. Demuxer I/O is aborted
    /// through `cancel` (see [`interrupt_cb`]).
    pub(super) fn open(cancel_token: ReadOnlyCancelToken, url: &str) -> Result<Self> {
        let url_c = CString::new(url).map_err(|_| anyhow!("url contains a NUL byte"))?;

        let input = unsafe {
            let mut fmt = ff::avformat_alloc_context();
            if fmt.is_null() {
                return Err(anyhow!("avformat_alloc_context failed"));
            }
            (*fmt).interrupt_callback = ff::AVIOInterruptCB {
                callback: Some(interrupt_cb),
                opaque: cancel_token.as_flag_ptr().cast_mut().cast::<c_void>(),
            };
            // avformat_open_input frees the context on failure
            check(
                "avformat_open_input",
                ff::avformat_open_input(&mut fmt, url_c.as_ptr(), ptr::null(), ptr::null_mut()),
            )?;
            Self {
                fmt,
                _cancel: cancel_token,
            }
        };

        check("avformat_find_stream_info", unsafe {
            ff::avformat_find_stream_info(input.fmt, ptr::null_mut())
        })?;
        Ok(input)
    }

    pub(super) const fn as_ptr(&self) -> *mut ff::AVFormatContext {
        self.fmt
    }

    pub(super) fn find_best_stream(&self, media_type: ff::AVMediaType) -> c_int {
        unsafe { ff::av_find_best_stream(self.fmt, media_type, -1, -1, ptr::null_mut(), 0) }
    }

    /// SAFETY: the streams belong to the context, which outlives the view
    pub(super) fn stream_at(&self, index: c_int) -> Stream {
        unsafe { Stream::from_raw(*(*self.fmt).streams.offset(index as isize)) }
    }

    /// Timestamp of the first frame in seconds; the media timeline is
    /// normalized so playback starts at 0.
    pub(super) fn start_offset(&self) -> f64 {
        let start_time = unsafe { (*self.fmt).start_time };
        if start_time == ff::AV_NOPTS_VALUE {
            0.0
        } else {
            start_time as f64 / f64::from(ff::AV_TIME_BASE)
        }
    }

    /// Total duration in seconds, `None` for realtime streams.
    pub(super) fn duration(&self) -> Option<f64> {
        let duration = unsafe { (*self.fmt).duration };
        if duration == ff::AV_NOPTS_VALUE || duration <= 0 {
            None
        } else {
            Some(duration as f64 / f64::from(ff::AV_TIME_BASE))
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        unsafe { ff::avformat_close_input(&mut self.fmt) };
    }
}
