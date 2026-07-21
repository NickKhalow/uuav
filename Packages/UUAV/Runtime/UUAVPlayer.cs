using System;
using UnityEngine;

namespace UUAV
{
    [RequireComponent(typeof(AudioSource))]
    public sealed class UUAVPlayer : MonoBehaviour
    {
        private delegate ResultFFI TimeQuery(ulong playerId, out double value);

        /// <summary>
        /// M stands for "Managed" mirrors the original MediaInfo and makes it serializable
        /// </summary>
        [Serializable]
        public struct MediaInfo_M
        {
            public double Duration;
            public double Framerate;
            public long VideoBitrate;
            public long AudioBitrate;
            public uint Width;
            public uint Height;
            public int SampleRate;
            public int Channels;
            public bool HasVideo;

            public bool HasAudio;

            // e.g. "h264"
            public string VideoCodec;

            // e.g. "yuv420p"
            public string PixelFormat;

            // e.g. "aac"
            public string AudioCodec;

            // e.g. "fltp"
            public string SampleFormat;

            public static MediaInfo_M From(MediaInfo mediaInfo)
            {
                return new MediaInfo_M
                {
                    Duration = mediaInfo.Duration,
                    Framerate = mediaInfo.Framerate,
                    VideoBitrate = mediaInfo.VideoBitrate,
                    AudioBitrate = mediaInfo.AudioBitrate,
                    Width = mediaInfo.Width,
                    Height = mediaInfo.Height,
                    SampleRate = mediaInfo.SampleRate,
                    Channels = mediaInfo.Channels,
                    HasVideo = mediaInfo.HasVideo,
                    HasAudio = mediaInfo.HasAudio,
                    VideoCodec = mediaInfo.VideoCodec,
                    PixelFormat = mediaInfo.PixelFormat,
                    AudioCodec = mediaInfo.AudioCodec,
                    SampleFormat = mediaInfo.SampleFormat
                };
            }
        }

        private static readonly int YTexId = Shader.PropertyToID("_YTex");
        private static readonly int UVTexId = Shader.PropertyToID("_UVTex");

        [SerializeField] private string url = "";
        [SerializeField] private bool playOnStart;

        // optional user-provided surface; auto-allocated at video size when null
        [SerializeField] private RenderTexture? targetTexture;

        [Header("Debug View")]
        // immutable after Awake; 0 means creation failed and the component is inert
        [SerializeField]
        private ulong playerId;

        [SerializeField] private int nativeChannels;
        [SerializeField] private VideoSize videoSize;

        [SerializeField] private bool enableDebugGather;
        [SerializeField] private double currentTimeDebug;
        [SerializeField] private double durationDebug;
        [SerializeField] private string? nativeState;
        [SerializeField] private MediaInfo_M mediaInfo;

        [Header("Resources")] [SerializeField] private Material? nv12Material;
        [SerializeField] private AudioSource audioSource = null!;
        [SerializeField] private Texture2D? yPlane;
        [SerializeField] private Texture2D? uvPlane;
        [SerializeField] private RenderTexture? runtimeSurface;
        [SerializeField] private IntPtr nativeTexture;

        private static ulong playerIncrementalID;

        public string CurrentUrl => url;

        public UUAVState State => NativeMethods.uuav_player_state(playerId);

        public double CurrentTime => ReadTime(NativeMethods.uuav_player_current_time);

        public double Duration => ReadTime(NativeMethods.uuav_player_duration);

        public RenderTexture? CurrentTexture =>
            targetTexture != null ? targetTexture : runtimeSurface;

        public AudioSource AudioSource => audioSource;

        public void OpenMedia(string mediaUrl)
        {
            url = mediaUrl;
            Check(NativeMethods.uuav_player_open_media_async(playerId, mediaUrl), "open media");
        }

        [ContextMenu(nameof(PrintState))]
        private void PrintState()
        {
            Debug.Log($"[UUAV] state: {NativeMethods.uuav_player_state(playerId).ToStringNoAlloc()}");
        }

        [ContextMenu(nameof(OpenMedia))]
        private void OpenMedia()
        {
            OpenMedia(url);
        }

        [ContextMenu(nameof(CloseMedia))]
        public void CloseMedia()
        {
            Check(NativeMethods.uuav_player_close_media(playerId), "close media");
        }

        [ContextMenu(nameof(Play))]
        public void Play()
        {
            Check(NativeMethods.uuav_player_play(playerId), "play");
        }

        [ContextMenu(nameof(Pause))]
        public void Pause()
        {
            Check(NativeMethods.uuav_player_pause(playerId), "pause");
        }

        public void Seek(double time)
        {
            Check(NativeMethods.uuav_player_seek_async(playerId, time), "seek");
        }

        public bool TryGetMediaInfo(out MediaInfo info)
        {
            info = default;
            if (playerId == 0)
            {
                return false;
            }

            if (State is UUAVState.Closed or UUAVState.Error)
            {
                return false;
            }

            var result = NativeMethods.uuav_player_get_media_info(playerId, out info);
            Check(result, "uuav_player_get_media_info");
            return result.IsOk;
        }

        // TODO if long-session drift corrections become audible, derive media
        // time from frames consumed in OnAudioFilterRead and feed 
        public void AssignMasterClock(double mediaTime)
        {
            Check(
                NativeMethods.uuav_player_assign_master_clock(playerId, mediaTime),
                "assign master clock"
            );
        }

        public static UUAVPlayer New()
        {
            ulong currentID = ++playerIncrementalID;
            GameObject gm = new GameObject($"UUAVPlayer_Instance_{currentID}");
            UUAVPlayer instance = gm.AddComponent<UUAVPlayer>();
            return instance;
        }

        private void Awake()
        {
            audioSource = GetComponent<AudioSource>();

            var shader = Shader.Find("Hidden/UUAV/NV12ToRGB");
            if (shader == null)
            {
                Debug.LogError("[UUAV] shader Hidden/UUAV/NV12ToRGB not found");
                return;
            }

            nv12Material = new Material(shader);

            var newPlayer = NativeMethods.uuav_player_new();
            if (newPlayer.IsOk == false)
            {
                Debug.LogError($"[UUAV] new player: {newPlayer.ConsumeError()}");
                return;
            }

            playerId = newPlayer.PlayerId;
            nativeChannels = NativeMethods.uuav_status().AudioOptions.Channels;

            audioSource.Play();
        }

        private void Start()
        {
            if (playOnStart && url.Length > 0)
            {
                OpenMedia(url);
                Play();
            }
        }

        private void Update()
        {
            if (playerId == 0)
            {
                return;
            }

            if (enableDebugGather)
            {
                durationDebug = Duration;
                currentTimeDebug = CurrentTime;
                nativeState = NativeMethods.uuav_player_state(playerId).ToStringNoAlloc();

                if (TryGetMediaInfo(out MediaInfo m))
                {
                    mediaInfo = MediaInfo_M.From(m);
                }
            }

            switch (NativeMethods.uuav_player_state(playerId))
            {
                case UUAVState.Ready:
                case UUAVState.Playing:
                case UUAVState.Paused:
                case UUAVState.Ended:
                    break;
                default:
                    return;
            }

            // presents the due frame into the native NV12 texture on the render
            // thread; the blit below is enqueued after it in submission order,
            // so it samples the freshly presented frame in the same frame
            GL.IssuePluginEvent(UUAVRuntime.RenderCallback, (int)playerId);

            if (RefreshVideoTexture())
            {
                BlitToSurface();
            }
        }

        private void OnDisable()
        {
            if (State == UUAVState.Playing)
            {
                Pause();
            }
        }

        private void OnDestroy()
        {
            audioSource.Stop();

            if (playerId != 0)
            {
                NativeMethods.uuav_player_free(playerId);
            }

            ReleasePlaneViews();

            if (runtimeSurface != null)
            {
                runtimeSurface.Release();
                Destroy(runtimeSurface);
                runtimeSurface = null;
            }

            if (nv12Material != null)
            {
                Destroy(nv12Material);
                nv12Material = null;
            }
        }

        #region AI generated

        // TODO AI generated, recheck carefully
        // audio mixer thread. playerId is immutable, safe to read directly;
        // a stale id after uuav_player_free is a native no-op returning 0
        private void OnAudioFilterRead(float[] data, int channels)
        {
            if (playerId == 0 || channels != nativeChannels)
            {
                // TODO on channel mismatch: uuav_update_audio_out to renegotiate
                Array.Clear(data, 0, data.Length);
                return;
            }

            var read = NativeMethods.uuav_player_read_audio(playerId, data, data.Length / channels);
            if (read == 0)
            {
                // missing/freed player leaves the buffer untouched
                Array.Clear(data, 0, data.Length);
            }
        }

        // TODO replace the per-frame poll with a native push notification once
        // the native side exposes one (see native/src/video_output.rs)
        private bool RefreshVideoTexture()
        {
            var textureResult = NativeMethods.uuav_player_get_video_texture(
                playerId,
                0,
                out var texture
            );
            if (textureResult.IsOk == false)
            {
                // expected until the first frame is presented
                textureResult.ConsumeError();
                return false;
            }

            var sizeResult = NativeMethods.uuav_player_get_video_size(playerId, out var size);
            if (sizeResult.IsOk == false)
            {
                sizeResult.ConsumeError();
                return false;
            }

            // the native texture is recreated on every resolution change,
            // including mid-play adaptive/IDR switches, so pointer + size
            // comparison catches first frame, re-open and mid-stream changes
            if (
                texture != nativeTexture
                || size.Width != videoSize.Width
                || size.Height != videoSize.Height
            )
            {
                RecreatePlaneViews(texture, size);
            }

            return yPlane != null;
        }

        private void RecreatePlaneViews(IntPtr texture, VideoSize size)
        {
            ReleasePlaneViews();

            var width = (int)size.Width;
            var height = (int)size.Height;

            // one NV12 resource, two SRVs: D3D11 selects the plane
            // from the view format (R8 -> Y, R8G8 -> UV)
            yPlane = Texture2D.CreateExternalTexture(
                width,
                height,
                TextureFormat.R8,
                mipChain: false,
                linear: true,
                texture
            );
            uvPlane = Texture2D.CreateExternalTexture(
                width / 2,
                height / 2,
                TextureFormat.RG16,
                mipChain: false,
                linear: true,
                texture
            );
            ConfigurePlane(yPlane);
            ConfigurePlane(uvPlane);

            if (targetTexture == null)
            {
                if (runtimeSurface != null)
                {
                    runtimeSurface.Release();
                    Destroy(runtimeSurface);
                }

                runtimeSurface = new RenderTexture(width, height, 0, RenderTextureFormat.ARGB32);
            }

            nativeTexture = texture;
            videoSize = size;
        }

        private void BlitToSurface()
        {
            var surface = CurrentTexture;
            if (surface == null || nv12Material == null)
            {
                return;
            }

            nv12Material.SetTexture(YTexId, yPlane);
            nv12Material.SetTexture(UVTexId, uvPlane);
            Graphics.Blit(null, surface, nv12Material);
        }

        private void ReleasePlaneViews()
        {
            // wrappers only: Unity drops its SRVs, the native resource is untouched
            if (yPlane != null)
            {
                Destroy(yPlane);
                yPlane = null;
            }

            if (uvPlane != null)
            {
                Destroy(uvPlane);
                uvPlane = null;
            }

            nativeTexture = IntPtr.Zero;
            videoSize = default;
        }

        private double ReadTime(TimeQuery query)
        {
            if (playerId == 0)
            {
                return 0;
            }

            var result = query(playerId, out var value);
            if (result.IsOk)
            {
                return value;
            }

            // unavailable is expected (no media open / realtime stream)
            result.ConsumeError();
            return 0;
        }

        private static void Check(ResultFFI result, string operation)
        {
            if (result.IsOk == false)
            {
                Debug.LogError($"[UUAV] {operation}: {result.ConsumeError()}");
            }
        }

        private static void ConfigurePlane(Texture2D plane)
        {
            plane.filterMode = FilterMode.Bilinear;
            plane.wrapMode = TextureWrapMode.Clamp;
        }

        #endregion
    }
}
