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
// TODO solve later
#![allow(clippy::significant_drop_tightening)]

mod audio_decoder;
mod ffutil;
mod hw_device;
mod player;
mod video_decoder;
mod video_output;

use anyhow::{Context as _, ensure};
use arc_swap::{ArcSwap, ArcSwapOption};
use dashmap::DashMap;
use hw_device::HwDevice;
use player::UUAVPlayer;
use std::{
    ffi::{CStr, CString},
    os::raw::{c_char, c_void},
    ptr,
    sync::{Arc, atomic::AtomicU64},
};

static INIT_STATE: ArcSwapOption<Runtime> = ArcSwapOption::const_empty();
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

struct Runtime {
    device: HwDevice,
    error_callback: ErrorCallback,
    audio_options: Arc<ArcSwap<AudioOptions>>,
    registry: DashMap<PlayerId, UUAVPlayer>,
}

pub type ErrorCallback = extern "C" fn(*const c_char);

pub type PlayerId = u64;

#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Clone, Copy)]
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
    pub audio_options: AudioOptions,
}

#[repr(C)]
#[derive(Default, Clone)]
pub struct AudioOptions {
    pub sample_rate: u32,
    pub channels: u32,
}

#[repr(C)]
#[derive(Clone)]
pub struct VideoSize {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
pub struct NewPlayerResult {
    pub player_id: PlayerId,
    pub error_message: *const c_char,
}

#[repr(C)]
pub struct ConsumeFrameResult {
    pub ptr: *const f32,
    pub len: i32,
    pub capacity: i32,
    pub error_message: *const c_char,
}

impl NewPlayerResult {
    const fn ok(player_id: PlayerId) -> Self {
        Self {
            player_id,
            error_message: ptr::null(),
        }
    }

    fn error(message: &str) -> Self {
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

fn string_to_c_bytes(s: &str) -> *const c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

const ERR_NO_RUNTIME: &str = "Runtime is not found";
const ERR_NO_PLAYER: &str = "player with specific id not found";

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

#[unsafe(no_mangle)]
pub extern "C" fn uuav_init(
    hw_device: *const c_void,
    audio_options: AudioOptions,
    error_callback: Option<ErrorCallback>,
) -> ResultFFI {
    if INIT_STATE.load().is_some() {
        return ResultFFI::error("Already initialized");
    }

    let Some(error_callback) = error_callback else {
        return ResultFFI::error("Error callback is null");
    };

    if hw_device.is_null() {
        return ResultFFI::error("HwDevice is null");
    }

    let new_runtime = Runtime {
        device: HwDevice::from_raw(hw_device),
        error_callback,
        audio_options: Arc::new(ArcSwap::new(Arc::new(audio_options))),
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
pub extern "C" fn uuav_update_audio_out(options: AudioOptions) -> ResultFFI {
    let state = INIT_STATE.load();

    if let Some(s) = state.as_ref() {
        s.audio_options.store(options.into());
        ResultFFI::ok()
    } else {
        ResultFFI::error("Not initialized")
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_status() -> Status {
    if let Some(s) = INIT_STATE.load().as_ref() {
        let audio_options = (**s.audio_options.load()).clone();
        let players_count = s.registry.len() as u64;

        Status {
            players_count,
            initialized: true,
            audio_options,
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

    let player = UUAVPlayer::new(s.device, Arc::clone(&s.audio_options), s.error_callback);
    let next_id = NEXT_STREAM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    s.registry.insert(next_id, player);

    NewPlayerResult::ok(next_id)
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
    runtime.player_by_id_mut(player_id)?.close_media()
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

// valid from READY; may be unavailable for realtime streams
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

// valid from READY
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
