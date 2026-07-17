using System;
using System.Text;
using AOT;
using UnityEngine;

namespace UUAV
{
    /// <summary>
    /// Owns the native runtime lifecycle
    /// </summary>
    internal static class UUAVRuntime
    {
        // TODO make sure to don't overabuse the calls from native parts using too many threads (we had a similar issue before)
        // rooted for the process lifetime: the native side keeps this fn pointer
        private static readonly ErrorCallback errorCallback = OnNativeError;

        private static IntPtr renderCallback;

        public static bool Initialized => NativeMethods.uuav_status().Initialized;

        // pass to GL.IssuePluginEvent with the player id as the event id
        public static IntPtr RenderCallback
        {
            get
            {
                if (renderCallback == IntPtr.Zero)
                {
                    renderCallback = NativeMethods.uuav_get_render_callback();
                }

                return renderCallback;
            }
        }

        [RuntimeInitializeOnLoadMethod(RuntimeInitializeLoadType.BeforeSceneLoad)]
        private static void Init()
        {
            if (Initialized)
            {
                return;
            }

            Application.quitting += Deinit;

            var config = AudioSettings.GetConfiguration();
            var audioOptions = AudioOptions.FromConfig(config);

            using var probe = ProbeTexture.New();
            var result = NativeMethods.uuav_init(probe.NativePtr(), audioOptions, errorCallback);

            if (result.IsOk == false)
            {
                Debug.LogError($"[UUAV] init: {result.ConsumeError()}");
            }


            // TODO react to AudioSettings.OnAudioConfigurationChanged:
            // uuav_update_audio_out and players rebuild their audio path
        }

        // Idempotent teardown. Resets the cached render callback so a
        // subsequent Init in a fresh domain re-fetches it from the DLL.
        private static void Deinit()
        {
            if (Initialized == false)
            {
                return;
            }

            NativeMethods.uuav_deinit();
            renderCallback = IntPtr.Zero;
        }

#if UNITY_EDITOR
        [UnityEditor.InitializeOnLoadMethod]
        private static void RegisterDomainReloadTeardown()
        {
            UnityEditor.AssemblyReloadEvents.beforeAssemblyReload -= Deinit;
            UnityEditor.AssemblyReloadEvents.beforeAssemblyReload += Deinit;
        }
#endif


        // called from native playback threads; reads only - native owns the string
        // Logging is thread-safe
        [MonoPInvokeCallback(typeof(ErrorCallback))]
        private static void OnNativeError(IntPtr message)
        {
            Debug.LogError($"[UUAV] {Utf8.PtrToString(message)}");
        }

#if UNITY_EDITOR
        [UnityEditor.MenuItem("UUAV/Init")]
        public static void ManualInit()
        {
            Init();
        }

        [UnityEditor.MenuItem("UUAV/Deinit")]
        public static void ManualDeinit()
        {
            Deinit();
        }
        
        [UnityEditor.MenuItem("UUAV/Status")]
        public static void PrintStats()
        {
            Status status = NativeMethods.uuav_status();
            
            StringBuilder sb = new StringBuilder();
            sb.AppendLine("[UUAV] Status:");
            sb.Append("Initialized: ").AppendLine(status.Initialized.ToString());
            sb.Append("Players Count: ").AppendLine(status.PlayersCount.ToString());
            sb.Append("Audio Channels: ").AppendLine(status.AudioOptions.Channels.ToString());
            sb.Append("Audio SampleRate: ").AppendLine(status.AudioOptions.SampleRate.ToString());
            sb.Append("Device Reason: ").AppendLine(status.ConsumeDeviceRemoveReason());
            sb.AppendLine("[UUAV] :Status");

            Debug.Log(sb.ToString());
        }
#endif

        private readonly struct ProbeTexture : IDisposable
        {
            private readonly Texture2D probe;

            private ProbeTexture(Texture2D probe)
            {
                this.probe = probe;
            }

            public static ProbeTexture New()
            {
                var probe = new Texture2D(1, 1, TextureFormat.RGBA32, mipChain: false);
                return new ProbeTexture(probe);
            }

            public void Dispose()
            {
                UnityEngine.Object.DestroyImmediate(probe);
            }

            public IntPtr NativePtr()
            {
                return probe.GetNativeTexturePtr();
            }
        }
    }
}
