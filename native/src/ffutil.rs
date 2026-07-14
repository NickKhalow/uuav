//! Thin RAII wrappers and error helpers over `ffmpeg-sys-next`.
//!
//! `ffmpeg-next` 8.1 does not compile against the bundled FFmpeg 8.1 headers,
//! so the pipeline talks to the sys bindings directly through these helpers.

use anyhow::{Result, anyhow};
use ffmpeg_sys_next as ff;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

/// Outcome of a receive call on a decoder.
pub(crate) enum Decoded<T> {
    Frame(T),
    /// Decoder needs more input.
    Again,
    /// Decoder is fully drained.
    Eof,
}

/// Formats an FFmpeg error code into an [`anyhow::Error`] with context.
pub(crate) fn av_err(context: &str, code: c_int) -> anyhow::Error {
    let mut buf = [0 as c_char; 64];
    let message = unsafe {
        if ff::av_strerror(code, buf.as_mut_ptr(), buf.len()) == 0 {
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            format!("ffmpeg error {code}")
        }
    };
    anyhow!("{context}: {message}")
}

/// Returns the code unchanged for non-negative FFmpeg return codes.
pub(crate) fn check(context: &str, code: c_int) -> Result<c_int> {
    if code < 0 {
        Err(av_err(context, code))
    } else {
        Ok(code)
    }
}

pub(crate) const fn q2d(r: ff::AVRational) -> f64 {
    if r.den == 0 {
        0.0
    } else {
        r.num as f64 / r.den as f64
    }
}

/// Owning wrapper around an `AVFrame`.
///
/// The frame's data buffers are reference counted by FFmpeg, so moving the
/// wrapper across threads (decoder thread to render thread) is safe.
pub(crate) struct OwnedFrame(*mut ff::AVFrame);

unsafe impl Send for OwnedFrame {}

impl OwnedFrame {
    pub(crate) fn new() -> Result<Self> {
        let ptr = unsafe { ff::av_frame_alloc() };
        if ptr.is_null() {
            return Err(anyhow!("av_frame_alloc failed"));
        }
        Ok(Self(ptr))
    }

    pub(crate) const fn as_ptr(&self) -> *const ff::AVFrame {
        self.0
    }

    #[allow(clippy::needless_pass_by_ref_mut)] // hands out a mutable alias
    pub(crate) const fn as_mut_ptr(&mut self) -> *mut ff::AVFrame {
        self.0
    }
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        unsafe { ff::av_frame_free(&mut self.0) };
    }
}

/// Owning wrapper around an `AVPacket`.
pub(crate) struct OwnedPacket(*mut ff::AVPacket);

impl OwnedPacket {
    pub(crate) fn new() -> Result<Self> {
        let ptr = unsafe { ff::av_packet_alloc() };
        if ptr.is_null() {
            return Err(anyhow!("av_packet_alloc failed"));
        }
        Ok(Self(ptr))
    }

    #[allow(clippy::needless_pass_by_ref_mut)] // hands out a mutable alias
    pub(crate) const fn as_mut_ptr(&mut self) -> *mut ff::AVPacket {
        self.0
    }

    pub(crate) fn stream_index(&self) -> c_int {
        unsafe { (*self.0).stream_index }
    }

    pub(crate) fn unref(&mut self) {
        unsafe { ff::av_packet_unref(self.0) };
    }
}

impl Drop for OwnedPacket {
    fn drop(&mut self) {
        unsafe { ff::av_packet_free(&mut self.0) };
    }
}
