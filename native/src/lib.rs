#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::todo,
    clippy::dbg_macro
)]
#![allow(
    clippy::uninlined_format_args,
    clippy::missing_errors_doc,
    clippy::option_if_let_else,
    clippy::single_match_else,
    clippy::must_use_candidate,
    clippy::future_not_send,
    clippy::enum_glob_use
)]
// FFI-heavy crate: raw-pointer borrows, numeric casts between C and media
// timestamp domains, and crate-private modules are pervasive by nature
// (doc_markdown: insists on backticks around the word "FFmpeg" in prose)
#![allow(
    clippy::borrow_as_ptr,
    clippy::redundant_pub_crate,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_ptr_alignment,
    clippy::missing_safety_doc,
    clippy::doc_markdown
)]

mod audio_decoder;
mod ffutil;
mod hw_device;
mod playback;
mod player;
mod video_decoder;
mod video_output;

use anyhow::{Context as _, ensure};
use arc_swap::{ArcSwap, ArcSwapOption};
use dashmap::DashMap;
use ffutil::StreamingProtocol;
use hw_device::HwDevice;
use player::UUAVPlayer;
use std::{
    convert::AsRef,
    ffi::{CStr, CString},
    num::{NonZeroI32, NonZeroUsize},
    os::raw::{c_char, c_void},
    ptr,
    sync::{Arc, Weak, atomic::AtomicU64},
};
use playback::DEFAULT_PLAYBACK_RATE;

const ERR_NO_RUNTIME: &str = "Runtime is not found";
const ERR_NO_PLAYER: &str = "player with specific id not found";

static INIT_STATE: ArcSwapOption<Runtime> = ArcSwapOption::const_empty();
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

struct Runtime {
    device: HwDevice,
    error_callback: Arc<RawErrorCallback>,
    audio_options: Arc<ArcSwap<AudioOptions>>,
    protocol_whitelist: Arc<StreamingProtocol>,
    registry: DashMap<PlayerId, UUAVPlayer>,
}

type RawErrorCallback = extern "C" fn(*const c_char);

// no-op when the raw callback is dropped on deinit
#[derive(Clone)]
struct ErrorCallback {
    callback: Weak<RawErrorCallback>,
}

impl ErrorCallback {
    pub fn from_raw(callback: Weak<RawErrorCallback>) -> Self {
        Self { callback }
    }

    pub fn report(&self, message: impl AsRef<str>) {
        if let Some(callback) = self.callback.upgrade() {
            let message: &str = message.as_ref();
            let c = CString::new(message).unwrap_or_default();
            callback(c.as_ptr());
        }
    }
}

pub type PlayerId = u64;

#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub enum UUAVState {
    UUAV_CLOSED = 0,
    UUAV_OPENING = 1,
    UUAV_READY = 2,
    UUAV_PLAYING = 3,
    UUAV_PAUSED = 4,
    UUAV_ENDED = 5,
    UUAV_ERROR = 6,
    UUAV_UNKNOWN = 7,
}

#[repr(C)]
#[derive(Default)]
pub struct Status {
    pub players_count: u64,
    pub initialized: bool,
    pub audio_options: AudioOptionsRaw,
    pub device_remove_reason: *const c_char,
}

/// Untrusted external input, must never be used for internals.
/// Always convert to AudioOptions to sanitize.
#[repr(C)]
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub struct AudioOptionsRaw {
    pub sample_rate: i32,
    pub channels: i32,
}

/// Sanitized input, guarantees to have values greater than zero
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AudioOptions {
    pub sample_rate: NonZeroI32,
    pub channels: NonZeroI32,
}

impl AudioOptions {
    // AudioOptions is sanitized at the FFI boundary: always positive
    const fn channels_usize(self) -> NonZeroUsize {
        // SAFETY the channels value is checked at the construction
        unsafe { NonZeroUsize::new_unchecked(self.channels.get() as usize) }
    }

    fn sample_rate_f64(self) -> f64 {
        f64::from(self.sample_rate.get())
    }
}

impl TryFrom<AudioOptionsRaw> for AudioOptions {
    type Error = anyhow::Error;

    fn try_from(raw: AudioOptionsRaw) -> Result<Self, Self::Error> {
        ensure!(
            raw.sample_rate > 0,
            "sample_rate must be positive, got {}",
            raw.sample_rate
        );
        ensure!(
            raw.channels > 0,
            "channels must be positive, got {}",
            raw.channels
        );

        unsafe {
            let sample_rate = NonZeroI32::new_unchecked(raw.sample_rate);
            let channels = NonZeroI32::new_unchecked(raw.channels);
            Ok(Self {
                sample_rate,
                channels,
            })
        }
    }
}

impl From<AudioOptions> for AudioOptionsRaw {
    fn from(options: AudioOptions) -> Self {
        Self {
            sample_rate: options.sample_rate.get(),
            channels: options.channels.get(),
        }
    }
}

/// Read-only view of the engine's audio output configuration
/// For internal look-ups
#[derive(Clone)]
pub(crate) struct AudioOptionsView(Arc<ArcSwap<AudioOptions>>);

impl AudioOptionsView {
    pub(crate) fn current(&self) -> AudioOptions {
        **self.0.load()
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct VideoSize {
    pub width: u32,
    pub height: u32,
}

/// Snapshot of the open media's source stream parameters.
///
/// Name fields are NUL-terminated UTF-8; =
/// unknown values are 0 / empty, `duration` is -1.0 for realtime streams.
/// Video fields are zero when `has_video` is 0,
/// audio fields when `has_audio` is 0.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MediaInfo {
    pub duration: f64,
    pub framerate: f64,
    pub video_bitrate: i64,
    pub audio_bitrate: i64,
    pub width: u32,
    pub height: u32,
    pub sample_rate: i32,
    pub channels: i32,
    pub video_codec: [c_char; MEDIA_INFO_NAME_LEN],
    pub pixel_format: [c_char; MEDIA_INFO_NAME_LEN],
    pub audio_codec: [c_char; MEDIA_INFO_NAME_LEN],
    pub sample_format: [c_char; MEDIA_INFO_NAME_LEN],
    pub has_video: u8,
    pub has_audio: u8,
}

pub const MEDIA_INFO_NAME_LEN: usize = 32;

impl MediaInfo {
    pub(crate) const fn empty() -> Self {
        Self {
            duration: -1.0,
            framerate: 0.0,
            video_bitrate: 0,
            audio_bitrate: 0,
            width: 0,
            height: 0,
            sample_rate: 0,
            channels: 0,
            video_codec: [0; MEDIA_INFO_NAME_LEN],
            pixel_format: [0; MEDIA_INFO_NAME_LEN],
            audio_codec: [0; MEDIA_INFO_NAME_LEN],
            sample_format: [0; MEDIA_INFO_NAME_LEN],
            has_video: 0,
            has_audio: 0,
        }
    }
}

/// Snapshot of the user-facing control values: the latest pushed
/// intents, which the playback thread applies asynchronously.
///
/// A `*_pending` flag of 1 means that control's latest push has not
/// been consumed by the playback thread yet. Each value/pending pair
/// is consistent; the controls are sampled independently of each other.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControlsState {
    pub rate: f64,
    pub play: u8,
    pub play_pending: u8,
    pub looping: u8,
    pub looping_pending: u8,
    pub rate_pending: u8,
}

#[repr(C)]
pub struct NewPlayerResult {
    pub player_id: PlayerId,
    pub error_message: *const c_char,
}

impl NewPlayerResult {
    const fn ok(player_id: PlayerId) -> Self {
        Self {
            player_id,
            error_message: ptr::null(),
        }
    }

    fn error(message: impl AsRef<str>) -> Self {
        Self {
            player_id: 0,
            error_message: string_to_c_bytes(message),
        }
    }
}

#[repr(C)]
pub struct ResultFFI {
    pub error_message: *const c_char,
}

impl ResultFFI {
    const fn ok() -> Self {
        Self {
            error_message: ptr::null(),
        }
    }

    fn error(message: &str) -> Self {
        Self {
            error_message: string_to_c_bytes(message),
        }
    }
}

impl<T> From<anyhow::Result<T>> for ResultFFI {
    fn from(value: anyhow::Result<T>) -> Self {
        match value {
            Ok(_) => Self::ok(),
            Err(e) => Self::error(e.to_string().as_str()),
        }
    }
}

impl From<anyhow::Error> for ResultFFI {
    fn from(err: anyhow::Error) -> Self {
        Self::error(err.to_string().as_str())
    }
}

fn string_to_c_bytes(s: impl AsRef<str>) -> *const c_char {
    CString::new(s.as_ref()).unwrap_or_default().into_raw()
}

impl Runtime {
    /// Shared-access guard over a registered player; the player stays
    /// solely owned by the registry, so the guard cannot outlive it.
    fn player_by_id(
        &self,
        player_id: PlayerId,
    ) -> anyhow::Result<dashmap::mapref::one::Ref<'_, PlayerId, UUAVPlayer>> {
        self.registry.get(&player_id).context(ERR_NO_PLAYER)
    }

    /// Exclusive-access counterpart of [`Self::player_by_id`].
    fn player_by_id_mut(
        &self,
        player_id: PlayerId,
    ) -> anyhow::Result<dashmap::mapref::one::RefMut<'_, PlayerId, UUAVPlayer>> {
        self.registry.get_mut(&player_id).context(ERR_NO_PLAYER)
    }
}

/// releases an error message
/// must be called exactly once per non-null message
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_string_free(string: *mut c_char) {
    if string.is_null() {
        return;
    }

    drop(unsafe { CString::from_raw(string) });
}

#[unsafe(no_mangle)]
pub const extern "C" fn uuav_abi_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), '\0').as_ptr().cast()
}

/// Initializes the global runtime.
///
/// `protocol_whitelist` is a NUL-terminated, comma-separated FFmpeg protocol
/// list; init fails if it is null or empty. Recommended baseline is
/// `https,http,tls,tcp,crypto,data,udp,rtp,rtcp,rtsp`, adding `file` only for
/// editor/local playback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_init(
    texture: *const c_void,
    audio_options: AudioOptionsRaw,
    error_callback: Option<RawErrorCallback>,
    protocol_whitelist: *const c_char,
) -> ResultFFI {
    if INIT_STATE.load().is_some() {
        return ResultFFI::error("Already initialized");
    }

    let Some(error_callback) = error_callback else {
        return ResultFFI::error("Error callback is null");
    };

    if texture.is_null() {
        return ResultFFI::error("Texture to capture the HwDevice from is not provided");
    }

    let device = match unsafe { hw_device::HwDevice::from_texture(texture) } {
        Ok(hw_device) => hw_device,
        Err(e) => return e.into(),
    };

    let audio_options = match AudioOptions::try_from(audio_options) {
        Ok(options) => options,
        Err(e) => return e.into(),
    };

    let protocol_whitelist = match unsafe { StreamingProtocol::new(protocol_whitelist) } {
        Ok(protocols) => protocols,
        Err(e) => return e.into(),
    };

    let new_runtime = Runtime {
        device,
        error_callback: Arc::new(error_callback),
        audio_options: Arc::new(ArcSwap::new(Arc::new(audio_options))),
        protocol_whitelist: Arc::new(protocol_whitelist),
        registry: DashMap::new(),
    };

    INIT_STATE.store(Some(Arc::new(new_runtime)));
    ResultFFI::ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_deinit() {
    INIT_STATE.store(None);
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_update_audio_out(options: AudioOptionsRaw) -> ResultFFI {
    let state = INIT_STATE.load();

    let Some(s) = state.as_ref() else {
        return ResultFFI::error("Not initialized");
    };

    match AudioOptions::try_from(options) {
        Ok(options) => {
            s.audio_options.store(Arc::new(options));
            ResultFFI::ok()
        }
        Err(e) => ResultFFI::error(&e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_status() -> Status {
    if let Some(s) = INIT_STATE.load().as_ref() {
        let audio_options = (**s.audio_options.load()).into();
        let players_count = s.registry.len() as u64;

        let device_remove_reason = match unsafe { s.device.device().GetDeviceRemovedReason() } {
            Ok(()) => ptr::null(),
            Err(e) => string_to_c_bytes(e.to_string()),
        };

        Status {
            players_count,
            initialized: true,
            audio_options,
            device_remove_reason,
        }
    } else {
        Status::default()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_new() -> NewPlayerResult {
    let state = INIT_STATE.load();
    let Some(s) = state.as_ref() else {
        return NewPlayerResult::error(ERR_NO_RUNTIME);
    };

    let next_id = NEXT_STREAM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match UUAVPlayer::new(
        next_id,
        s.device.clone(),
        AudioOptionsView(Arc::clone(&s.audio_options)),
        ErrorCallback::from_raw(Arc::downgrade(&s.error_callback)),
        Arc::clone(&s.protocol_whitelist),
    ) {
        Ok(player) => {
            s.registry.insert(next_id, player);
            NewPlayerResult::ok(next_id)
        }
        Err(e) => NewPlayerResult::error(e.to_string()),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_free(player_id: PlayerId) {
    let state = INIT_STATE.load();
    let Some(s) = state.as_ref() else {
        return;
    };

    s.registry.remove(&player_id);
}

// ---- lifecycle -------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_play(player_id: PlayerId) -> ResultFFI {
    uuav_player_play_internal(player_id).into()
}

fn uuav_player_play_internal(player_id: PlayerId) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id(player_id)?.play()
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_pause(player_id: PlayerId) -> ResultFFI {
    uuav_player_pause_internal(player_id).into()
}

fn uuav_player_pause_internal(player_id: PlayerId) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id(player_id)?.pause()
}

// async! returns immediately
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_open_media_async(
    player_id: PlayerId,
    url: *const c_char,
) -> ResultFFI {
    unsafe { uuav_player_open_media_async_internal(player_id, url) }.into()
}

unsafe fn uuav_player_open_media_async_internal(
    player_id: PlayerId,
    url: *const c_char,
) -> anyhow::Result<()> {
    ensure!(!url.is_null(), "url is null");
    let url = unsafe { CStr::from_ptr(url) }
        .to_str()
        .context("url is not valid UTF-8")?
        .to_owned();
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id_mut(player_id)?.open_media_intent(url)
}

// back to CLOSED, player reusable
#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_close_media(player_id: PlayerId) -> ResultFFI {
    uuav_player_close_media_internal(player_id).into()
}

fn uuav_player_close_media_internal(player_id: PlayerId) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id_mut(player_id)?.close_media();
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_state(player_id: PlayerId) -> UUAVState {
    let state = INIT_STATE.load();
    let Some(runtime) = state.as_ref() else {
        return UUAVState::UUAV_UNKNOWN;
    };

    runtime
        .player_by_id(player_id)
        .map_or(UUAVState::UUAV_UNKNOWN, |player| player.state())
}

// may be unavailable for realtime streams
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_duration(
    player_id: PlayerId,
    out_duration: *mut f64,
) -> ResultFFI {
    unsafe { uuav_player_duration_internal(player_id, out_duration) }.into()
}

unsafe fn uuav_player_duration_internal(
    player_id: PlayerId,
    out_duration: *mut f64,
) -> anyhow::Result<()> {
    ensure!(!out_duration.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let duration = runtime
        .player_by_id(player_id)?
        .duration()
        .context("duration is not available")?;
    unsafe { out_duration.write(duration) };
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_current_controls_state(
    player_id: PlayerId,
    out_state: *mut ControlsState,
) -> ResultFFI {
    unsafe { uuav_player_current_controls_state_internal(player_id, out_state) }.into()
}

unsafe fn uuav_player_current_controls_state_internal(
    player_id: PlayerId,
    out_state: *mut ControlsState,
) -> anyhow::Result<()> {
    ensure!(!out_state.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let controls = runtime.player_by_id(player_id)?.controls_state();
    unsafe { out_state.write(controls) };
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_current_time(
    player_id: PlayerId,
    out_time: *mut f64,
) -> ResultFFI {
    unsafe { uuav_player_current_time_internal(player_id, out_time) }.into()
}

unsafe fn uuav_player_current_time_internal(
    player_id: PlayerId,
    out_time: *mut f64,
) -> anyhow::Result<()> {
    ensure!(!out_time.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let time = runtime
        .player_by_id(player_id)?
        .current_time()
        .context("current time is not available")?;
    unsafe { out_time.write(time) };
    Ok(())
}

// the player slaves its playback to the externally provided master clock
#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_assign_master_clock(
    player_id: PlayerId,
    current_time: f64,
) -> ResultFFI {
    uuav_player_assign_master_clock_internal(player_id, current_time).into()
}

fn uuav_player_assign_master_clock_internal(
    player_id: PlayerId,
    current_time: f64,
) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime
        .player_by_id(player_id)?
        .assign_master_clock(current_time)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_get_video_size(
    player_id: PlayerId,
    out_size: *mut VideoSize,
) -> ResultFFI {
    unsafe { uuav_player_get_video_size_internal(player_id, out_size) }.into()
}

unsafe fn uuav_player_get_video_size_internal(
    player_id: PlayerId,
    out_size: *mut VideoSize,
) -> anyhow::Result<()> {
    ensure!(!out_size.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let size = runtime
        .player_by_id(player_id)?
        .video_size()
        .context("video size is not available yet")?;
    unsafe { out_size.write(size) };
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_get_media_info(
    player_id: PlayerId,
    out_info: *mut MediaInfo,
) -> ResultFFI {
    unsafe { uuav_player_get_media_info_internal(player_id, out_info) }.into()
}

unsafe fn uuav_player_get_media_info_internal(
    player_id: PlayerId,
    out_info: *mut MediaInfo,
) -> anyhow::Result<()> {
    ensure!(!out_info.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let info = runtime
        .player_by_id(player_id)?
        .media_info()
        .context("media info is not available yet")?;
    unsafe { out_info.write(info) };
    Ok(())
}

// ---- transport (commands: set flags, decoder thread obeys) ----------

// async; coalesces repeated calls
#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_seek_async(player_id: PlayerId, time: f64) -> ResultFFI {
    uuav_player_seek_async_internal(player_id, time).into()
}

fn uuav_player_seek_async_internal(player_id: PlayerId, time: f64) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id(player_id)?.seek_intent(time)
}

// persists across url switches
#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_set_looping(player_id: PlayerId, looping: u8) -> ResultFFI {
    uuav_player_set_looping_internal(player_id, looping != 0).into()
}

fn uuav_player_set_looping_internal(player_id: PlayerId, looping: bool) -> anyhow::Result<()> {
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id(player_id)?.set_looping(looping);
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_get_looping(player_id: PlayerId) -> u8 {
    let state = INIT_STATE.load();
    let Some(runtime) = state.as_ref() else {
        return 0;
    };
    runtime
        .player_by_id(player_id)
        .map_or(0, |player| u8::from(player.looping()))
}

// Expect: realtime streams (no duration) keep playing at 1x
#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_set_rate(player_id: PlayerId, rate: f64) -> ResultFFI {
    uuav_player_set_rate_internal(player_id, rate).into()
}

fn uuav_player_set_rate_internal(player_id: PlayerId, rate: f64) -> anyhow::Result<()> {
    ensure!(
        rate.is_finite() && rate > 0.0,
        "playback rate must be finite and positive, got {rate}"
    );
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    runtime.player_by_id(player_id)?.set_rate(rate);
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_get_rate(player_id: PlayerId) -> f64 {
    let state = INIT_STATE.load();
    let Some(runtime) = state.as_ref() else {
        return DEFAULT_PLAYBACK_RATE;
    };
    runtime
        .player_by_id(player_id)
        .map_or(DEFAULT_PLAYBACK_RATE, |player| player.rate())
}

// ---- video -----------------------------------------------------------

// Unity's UnityRenderingEvent signature
pub type UUAVRenderEvent = extern "C" fn(event_id: i32);

// [render] entry point issued via GL.IssuePluginEvent;
// `event_id` routes to the player with the matching id
extern "C" fn uuav_render_event(event_id: i32) {
    let Ok(player_id) = PlayerId::try_from(event_id) else {
        return;
    };

    let state = INIT_STATE.load();
    let Some(runtime) = state.as_ref() else {
        return;
    };

    if let Ok(player) = runtime.player_by_id(player_id) {
        player.on_render_event();
    }
}

// pass to GL.IssuePluginEvent
#[unsafe(no_mangle)]
pub extern "C" fn uuav_get_render_callback() -> UUAVRenderEvent {
    uuav_render_event
}

// NV12 ID3D11Texture2D; the engine creates its own plane views over it
// (`plane` is accepted for ABI compatibility and ignored). Valid from the
// first presented frame; recreated on resolution change — re-query then.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_get_video_texture(
    player_id: PlayerId,
    plane: i32,
    out_texture: *mut *const c_void,
) -> ResultFFI {
    let _ = plane;
    unsafe { uuav_player_get_video_texture_internal(player_id, out_texture) }.into()
}

unsafe fn uuav_player_get_video_texture_internal(
    player_id: PlayerId,
    out_texture: *mut *const c_void,
) -> anyhow::Result<()> {
    ensure!(!out_texture.is_null(), "out pointer is null");
    let state = INIT_STATE.load();
    let runtime = state.as_ref().context(ERR_NO_RUNTIME)?;
    let texture = runtime
        .player_by_id(player_id)?
        .video_texture()
        .context("video texture is not available yet")?;
    unsafe { out_texture.write(texture) };
    Ok(())
}

// ---- audio -----------------------------------------------------------

// [audio] fills interleaved FLT; pads silence on underrun,
// never blocks; returns frames actually copied
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_read_audio(
    player_id: PlayerId,
    dst: *mut f32,
    nb_frames: i32,
) -> i32 {
    if dst.is_null() || nb_frames <= 0 {
        return 0;
    }

    let state = INIT_STATE.load();
    let Some(runtime) = state.as_ref() else {
        return 0;
    };

    runtime
        .player_by_id(player_id)
        .map_or(0, |player| player.read_audio(dst, nb_frames))
}
