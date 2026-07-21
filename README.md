# UUAV

A video/audio player for Unity built on a native Rust core and FFmpeg, with D3D11VA hardware-accelerated video decoding.

- `native/` - Rust crate compiled to `uuav.dll` (`cdylib`, C ABI)
- `Packages/UUAV/` - Unity package (`com.nickkhalow.uuav`) with the C# bindings and the `UUAVPlayer` component

## How it works

The C# side is a thin binding layer. `UUAVRuntime` initializes the native runtime once at startup: it passes a probe texture (that's how the plugin captures Unity's D3D11 device), the audio output format and an error callback. Everything else goes through `UUAVPlayer`, a MonoBehaviour that talks to the native player by id.

On the native side each player owns a dedicated playback thread that demuxes and decodes the media. Video is decoded on the GPU via D3D11VA. When a frame is due, Unity's render thread (via `GL.IssuePluginEvent`) triggers a copy of the decoded surface into a shared NV12 texture; C# wraps its Y/UV planes with `Texture2D.CreateExternalTexture` and blits them to a regular `RenderTexture` through the `Hidden/UUAV/NV12ToRGB` shader. That `RenderTexture` is what you get from `player.CurrentTexture`.

Audio is decoded and resampled to interleaved `f32` at the engine's output format, buffered in a lock-free ring, and pulled by Unity from `OnAudioFilterRead`. Playback is paced by a master media clock, which can also be slaved to an external clock via `AssignMasterClock` (useful for syncing multiple players).

Opening a URL is async and cancellable; closing never blocks the engine - playback threads are cancelled and abandoned, never joined on the hot path.

### Basic usage

```csharp
var player = UUAVPlayer.New();      // or add the component in the editor
player.OpenMedia("https://example.com/video.mp4");
player.Play();

// render player.CurrentTexture (RenderTexture) anywhere - UI RawImage, material, etc.
```

See `Packages/UUAV/Example/UIExamplePlayer.cs` for a working example.

## Why Rust

Mostly safety: this is code that shares a D3D11 device and threads with the engine, and memory/thread-safety bugs there are miserable to debug. Rust catches most of them at compile time; the unsafe surface is confined to the FFI boundary, FFmpeg calls and D3D interop.

The other reason is reusability. The output is a plain C-ABI DLL with nothing engine-specific baked in - the same `uuav.dll` can be embedded in other engines or apps (Unreal, Godot, whatever), only the thin C# binding layer is Unity-specific.

## Building

Prerequisites:

- Rust toolchain with the GNU target: `rustup target add x86_64-pc-windows-gnu`
- MSYS2 with the mingw64 toolchain at `C:\msys64` (the linker is pinned to `C:\msys64\mingw64\bin\gcc.exe` in `native/.cargo/config.toml`)
- FFmpeg **8.1** development files (headers + import libs) in `native/third_party/ffmpeg/`. Use the [BtbN](https://github.com/BtbN/FFmpeg-Builds/releases) **LGPL shared** win64 build. `FFMPEG_DIR` already points there via `native/.cargo/config.toml`.

Then:

```bash
cd native
./build.sh
```

This runs `cargo build --release --target x86_64-pc-windows-gnu` and copies `uuav.dll` into `Packages/UUAV/Runtime/Plugins/x86_64/`.

## Runtime deployment

`uuav.dll` dynamically links FFmpeg, so the FFmpeg runtime DLLs must sit next to `uuav.dll` (`Packages/UUAV/Runtime/Plugins/x86_64/` in-project, or the plugins folder of a built player). The exact set, taken from the FFmpeg **8.1** BtbN LGPL shared build:

```
avcodec-63.dll
avdevice-63.dll
avfilter-12.dll
avformat-63.dll
avutil-61.dll
libbz2-1.dll
libiconv-2.dll
liblzma-5.dll
libwinpthread-1.dll
swresample-7.dll
swscale-10.dll
zlib1.dll
```

The library major versions (avcodec **63**, avformat **63**, avutil **61**, swresample **7**, swscale **10**, avfilter **12**, avdevice **63**) are tied to the FFmpeg 8.1 release. A different FFmpeg version ships differently-named DLLs (`avcodec-62.dll`, …) that `uuav.dll` won't load - keep the DLL set and `ffmpeg-sys-next = "8.1.0"` in `native/Cargo.toml` in lockstep.

A few notes:

- `libwinpthread-1.dll` is mandatory. The FFmpeg chain links against it; if it isn't next to the plugin, the loader falls back to `PATH` (which may hold an incompatible copy, e.g. Git's older mingw64) and `LoadLibrary` fails with `ERROR_PROC_NOT_FOUND (127)`.
- FFmpeg is used under LGPL as a dynamically-linked shared library; see the license shipped with the BtbN build.

## GPU device synchronization

The native decoder shares Unity's D3D11 device (captured from a probe texture at init, see `native/src/hw_device.rs`). The `ID3D11Device` itself is free-threaded, but the immediate context is not - D3D11VA decode calls on the playback thread would otherwise race Unity's render thread and hang the runtime.

`SetMultithreadProtected(true)` used for the immediate context, performance affection is negligible in practice. All it does is wrap each context call in an internal critical section: uncontended that's nanoseconds, and contention only happens during the short window where a decoded frame is copied into the presentation texture. Next to the actual GPU work and Unity's own command submission this is noise based on the investigation.

The alternative - a separate D3D11 device for FFmpeg with frames passed across via `D3D11_RESOURCE_MISC_SHARED` textures - was considered and rejected. It removes immediate-context contention entirely, but adds cross-device texture lifetime management, per-frame open/acquire/release complexity and additional failure surface, all to avoid a cost that was already negligible.
