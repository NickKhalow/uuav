using UnityEngine;
using System;
using UUAV;

namespace RenderHeads.Media.AVProVideo
{
    internal sealed class UUAVBackend : IMediaControl, IMediaInfo, ITextureProducer
    {
        private static readonly TimeRanges NotBuffered = new TimeRanges(0);
        private static readonly TimeRanges Buffered = new TimeRanges(1);

        private readonly UUAVPlayer player;

        private bool looping;
        private float playbackRate = 1f;
        private bool seekRequested;

        public UUAVBackend(UUAVPlayer player)
        {
            this.player = player;
        }

        public void Tick()
        {
            UUAVState state = player.State;

            if (seekRequested
                && (state == UUAVState.Playing
                    || state == UUAVState.Paused
                    || state == UUAVState.Ready))
            {
                seekRequested = false;
            }

            // TODO implement native looping (avoid the decoder drainage and keep the decoder warm)
            if (looping && state == UUAVState.Ended)
            {
                player.Seek(0);
                player.Play();
            }
        }

        public void Play() => player.Play();

        public void Pause() => player.Pause();

        public void Stop() => player.Pause();

        public void Seek(double time)
        {
            seekRequested = true;
            player.Seek(time);
        }

        public bool IsPlaying() => player.State == UUAVState.Playing;

        public bool IsPaused() => player.State == UUAVState.Paused;

        // Best-effort: UUAV has no distinct seeking state, so this reflects a
        // pending Seek until the player settles (see Tick).
        public bool IsSeeking() => seekRequested;

        public bool IsBuffering() => player.State == UUAVState.Opening;

        public bool IsFinished() => player.State == UUAVState.Ended;

        public bool IsLooping() => looping;

        public void SetLooping(bool value) => looping = value;

        public double GetCurrentTime() => player.CurrentTime;

        public float GetPlaybackRate() => playbackRate;


        // TODO
        // Implement playback speed in UUAV, no-op for realtime streams (or only applied for slow playback? but what about the buffering?)
        public void SetPlaybackRate(float rate) => new NotImplementedException();

        public ErrorCode GetLastError() =>
            player.State == UUAVState.Error ? ErrorCode.LoadFailed : ErrorCode.None;

        // The consumer reads only Count, waiting for a buffered range to exist
        // before applying loop/rate/volume. Report one range once media is ready.
        public TimeRanges GetBufferedTimes()
        {
            switch (player.State)
            {
                case UUAVState.Ready:
                case UUAVState.Playing:
                case UUAVState.Paused:
                case UUAVState.Ended:
                    return Buffered;
                default:
                    return NotBuffered;
            }
        }

        public double GetDuration() => player.Duration;

        public Texture? GetTexture(int index = 0) 
        {
            return player.CurrentTexture;
        }

        // UUAV's NV12ToRGB shader already flips vertically, so the output
        // RenderTexture is upright and no consumer-side flip is needed.
        // Comment is valid as long as behaviour of the UUAVPlayer and shader remain unchanged
        public bool RequiresVerticalFlip() => false;
    }
}
