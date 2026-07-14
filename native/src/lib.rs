use anyhow::{Context, Result, anyhow};
use arc_swap::{ArcSwap, ArcSwapOption};
use crossbeam_channel::{Receiver, Sender, bounded};
use dashmap::DashMap;
use lazy_static::lazy_static;
use std::{
    ffi::{CStr, CString},
    os::raw::{c_char, c_void},
    ptr::{self, null},
    sync::{Arc, atomic::AtomicU64},
};

lazy_static! {
    static ref INIT_STATE: ArcSwapOption<Runtime> = ArcSwapOption::empty();
    static ref NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);
    //static ref DATA_CHANNELS: DashMap<u64, (Sender<Vec<f32>>, Receiver<Vec<f32>>)> = DashMap::new();
}

struct UUAVPlayer {
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

#[allow(non_camel_case_types)]
enum UUAVState {
    UUAV_CLOSED,
    UUAV_OPENING,
    UUAV_READY,
    UUAV_PLAYING,
    UUAV_PAUSED,
    UUAV_ENDED,
    UUAV_ERROR,
}

impl UUAVPlayer {
    fn new(device: HwDevice) -> Self {
        // TODO initialisation?
        todo!();

        Self { device }
    }

    // not blocking
    // async, returns immidieatly
    fn open_media_intent(&mut self, url: String) -> anyhow::Result<()> {
        todo!();
    }

    fn close_media(&mut self) -> anyhow::Result<()> {
        todo!();
    }

    fn play(&self) -> anyhow::Result<()> {
        todo!();
    }

    fn pause(&self) -> anyhow::Result<()> {
        todo!();
    }

    // not blocking
    fn seek_intent(&self, time: f64) -> anyhow::Result<()> {
        todo!()
    }

    fn state(&self) -> UUAVState {
        todo!();
    }

    fn duration(&self) -> anyhow::Result<Option<f64>> {
        todo!()
    }

    fn current_time(&self) -> anyhow::Result<Option<f64>> {
        todo!()
    }

    fn video_size() -> anyhow::Result<Option<VideoSize>> {
        todo!()
    }

    /*

    // ---- transport (commands: set flags, decoder thread obeys) ----------
    int         uuav_player_play(UUAVPlayer* p);            // [main]
    int         uuav_player_pause(UUAVPlayer* p);           // [main]
    int         uuav_player_seek_async(UUAVPlayer* p, double time);  // [main] async; coalesces repeated calls
        */
}

// Is guaranteed to live the whole lifecycle of the app, static lifetime
// D3D or Metal device
#[derive(Clone, Copy)]
struct HwDevice {
    // is passed from the master flow and remains valid for the whole
    // lifetime of the app
    raw_device: *const c_void,
}

unsafe impl Send for HwDevice {}
unsafe impl Sync for HwDevice {}

impl HwDevice {
    fn from_raw(raw_device: *const c_void) -> Self {
        Self { raw_device }
    }
}

struct Runtime {
    device: HwDevice,
    error_callback: ErrorCallback,
    audio_options: ArcSwap<AudioOptions>,
    registry: DashMap<PlayerId, UUAVPlayer>,
}

pub type ErrorCallback = extern "C" fn(*const c_char);

pub type PlayerId = u64;

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
#[derive(Default, Clone)]
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
    fn ok(player_id: PlayerId) -> Self {
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
    fn ok() -> Self {
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

fn vec_to_c_array(strings: Vec<String>) -> *const *const c_char {
    let cstrings: Vec<*const c_char> = strings.into_iter().map(|s| string_to_c_bytes(&s)).collect();

    let boxed_slice = cstrings.into_boxed_slice();

    let ptr = boxed_slice.as_ptr();
    std::mem::forget(boxed_slice);

    ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn uuav_abi_version() -> *const c_char {
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

    let error_callback = match error_callback {
        Some(e) => e,
        None => {
            return ResultFFI::error("Error callback is null");
        }
    };

    if hw_device.is_null() {
        return ResultFFI::error("HwDevice is null");
    }

    let new_runtime = Runtime {
        device: HwDevice::from_raw(hw_device),
        error_callback,
        audio_options: ArcSwap::new(Arc::new(audio_options)),
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
        return NewPlayerResult::error("Runtime is not found");
    };

    let player = UUAVPlayer::new(s.device);
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

#[unsafe(no_mangle)]
pub extern "C" fn uuav_player_play(player_id: PlayerId) -> ResultFFI {
    let state = INIT_STATE.load();
    let Some(state) = state.as_ref() else {
        return ResultFFI::error("Runtime is not found");
    };

    let player = state.registry.get(&player_id);
    let Some(player) = player else {
        return ResultFFI::error("stream with specified id not found");
    };

    player.play().into()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn uuav_player_pause(player_id: PlayerId) -> ResultFFI {
    todo!()
}

/*
// ---- lifecycle -------------------------------------------------------
int         uuav_player_open_media_async(UUAVPlayer* p, const char* url);  // [main] async! returns immediately
void        uuav_player_close_media(UUAVPlayer* p);     // [main] back to CLOSED, player reusable

// STATE - caller MUST provide a valid pointer to a player instance

typedef enum { UUAV_CLOSED, UUAV_OPENING, UUAV_READY, UUAV_PLAYING,
             UUAV_PAUSED, UUAV_ENDED, UUAV_ERROR } UUAVState;

UUAVState   uuav_player_state(UUAVPlayer* p);       // [any]
int         uuav_player_last_error(UUAVPlayer* p, char* buf, int buf_size); // [main]
double      uuav_player_duration(UUAVPlayer* p);    // [any] NaN until READY (what about realtime streams?)
double      uuav_player_current_time(UUAVPlayer* p);        // [any] current master-clock position
int         uuav_player_get_video_size(UUAVPlayer* p, int* w, int* h); // [main] valid from READY

// ---- transport (commands: set flags, decoder thread obeys) ----------
int         uuav_player_play(UUAVPlayer* p);            // [main]
int         uuav_player_pause(UUAVPlayer* p);           // [main]
int         uuav_player_seek_async(UUAVPlayer* p, double time);  // [main] async; coalesces repeated calls

// ---- video -----------------------------------------------------------
typedef void (*UUAVRenderEvent)(int event_id);          // Unity's UnityRenderingEvent signature
UUAVRenderEvent uuav_get_render_callback(void);         // [any]  pass to GL.IssuePluginEvent
int         uuav_player_get_id(UUAVPlayer* p);          // [any]  the event_id routing the callback to this player

void*       uuav_player_get_video_texture(UUAVPlayer* p, int plane); // [main] SRV: plane 0 = Y, 1 = UV
                                                      // valid from READY; recreated on resolution change

// ---- audio -----------------------------------------------------------
int         uuav_player_read_audio(UUAVPlayer* p, float* dst, int nb_frames);
                                                      // [audio] fills interleaved FLT; pads silence on underrun,
                                                      //         never blocks; returns frames actually copied

*/
