using System;
using System.Runtime.InteropServices;
using System.Text;

namespace UUAV
{
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void ErrorCallback(IntPtr message);

    public enum UUAVState
    {
        Closed,
        Opening,
        Ready,
        Playing,
        Paused,
        Ended,
        Error,
        Unknown,
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct AudioOptions
    {
        public int SampleRate;
        public int Channels;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct VideoSize
    {
        public uint Width;
        public uint Height;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Status
    {
        public ulong PlayersCount;
        private readonly byte initialized;
        public AudioOptions AudioOptions;

        public bool Initialized => initialized != 0;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct NewPlayerResult
    {
        public ulong PlayerId;

        // allocated by the native side; release via ConsumeError
        public IntPtr ErrorMessage;

        public bool IsOk => ErrorMessage == IntPtr.Zero;

        // reads the message and frees it on the native side; call at most once
        public string? ConsumeError()
        {
            var message = Utf8.PtrToString(ErrorMessage);
            NativeMethods.uuav_string_free(ErrorMessage);
            ErrorMessage = IntPtr.Zero;
            return message;
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct ResultFFI
    {
        // allocated by the native side; release via ConsumeError
        public IntPtr ErrorMessage;

        public bool IsOk => ErrorMessage == IntPtr.Zero;

        // reads the message and frees it on the native side; call at most once
        public string? ConsumeError()
        {
            var message = Utf8.PtrToString(ErrorMessage);
            NativeMethods.uuav_string_free(ErrorMessage);
            ErrorMessage = IntPtr.Zero;
            return message;
        }
    }

    internal static class NativeMethods
    {
        private const string Lib = "uuav";

        // must be called exactly once per non-null message
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern void uuav_string_free(IntPtr str);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr uuav_abi_version();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_init(
            IntPtr hwDevice,
            AudioOptions audioOptions,
            ErrorCallback? errorCallback
        );

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern void uuav_deinit();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_update_audio_out(AudioOptions options);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern Status uuav_status();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern NewPlayerResult uuav_player_new();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern void uuav_player_free(ulong playerId);

        // ---- lifecycle -------------------------------------------------

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_play(ulong playerId);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_pause(ulong playerId);

        // async! returns immediately
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        private static extern ResultFFI uuav_player_open_media_async(ulong playerId, IntPtr url);

        public static ResultFFI uuav_player_open_media_async(ulong playerId, string url)
        {
            var urlPtr = Utf8.AllocCString(url);
            try
            {
                return uuav_player_open_media_async(playerId, urlPtr);
            }
            finally
            {
                Marshal.FreeHGlobal(urlPtr);
            }
        }

        // back to CLOSED, player reusable
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_close_media(ulong playerId);

        // ---- state -----------------------------------------------------

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern UUAVState uuav_player_state(ulong playerId);

        // valid from READY; may be unavailable for realtime streams
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_duration(ulong playerId, out double duration);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_current_time(ulong playerId, out double time);

        // the player slaves its playback to the externally provided master clock
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_assign_master_clock(
            ulong playerId,
            double currentTime
        );

        // valid from READY
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_get_video_size(
            ulong playerId,
            out VideoSize size
        );

        // ---- transport (commands: set flags, decoder thread obeys) ------

        // async; coalesces repeated calls
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_seek_async(ulong playerId, double time);

        // ---- video -------------------------------------------------------

        // pass to GL.IssuePluginEvent / CommandBuffer.IssuePluginEvent
        // with the player id as the event id
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr uuav_get_render_callback();

        // SRV: plane 0 = Y, 1 = UV; valid from READY; recreated on resolution change
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern ResultFFI uuav_player_get_video_texture(
            ulong playerId,
            int plane,
            out IntPtr texture
        );

        // ---- audio -------------------------------------------------------

        // [audio] fills interleaved FLT; pads silence on underrun,
        // never blocks; returns frames actually copied
        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        public static extern int uuav_player_read_audio(
            ulong playerId,
            [Out] float[] dst,
            int nbFrames
        );
    }

    internal static class Utf8
    {
        public static unsafe string? PtrToString(IntPtr ptr)
        {
            if (ptr == IntPtr.Zero)
            {
                return null;
            }

            var bytes = (byte*)ptr;
            var length = 0;
            while (bytes[length] != 0)
            {
                length++;
            }

            return Encoding.UTF8.GetString(bytes, length);
        }

        public static IntPtr AllocCString(string value)
        {
            var bytes = Encoding.UTF8.GetBytes(value);
            var ptr = Marshal.AllocHGlobal(bytes.Length + 1);
            Marshal.Copy(bytes, 0, ptr, bytes.Length);
            Marshal.WriteByte(ptr, bytes.Length, 0);
            return ptr;
        }
    }
}
