import { useAudioState } from "@/lib/audio-state-context";
import { useAudioPlayback } from "@/lib/audio-playback-context";
import { clampTime } from "@/lib/format";

/**
 * Duration, display time and progress for every player. Prefers the engine's
 * duration over metadata (RSS durations are unreliable), shows saved progress
 * for audiobooks and podcasts before playback starts, and clamps everything.
 */
export function usePlaybackProgress() {
    const {
        currentTrack,
        currentAudiobook,
        currentPodcast,
        playbackType,
    } = useAudioState();

    const {
        currentTime,
        duration: playbackDuration,
    } = useAudioPlayback();

    const isMultiTrack = playbackType === "audiobook" && (currentAudiobook?.tracks?.length ?? 0) > 1;

    const duration = isMultiTrack
        ? (currentAudiobook?.duration || playbackDuration || 0)
        : playbackDuration > 0
            ? playbackDuration
            : (currentTrack?.duration || currentAudiobook?.duration || currentPodcast?.duration || 0);

    let time = currentTime;
    if (time <= 0) {
        if (playbackType === "audiobook" && currentAudiobook?.progress?.currentTime) {
            time = currentAudiobook.progress.currentTime;
        } else if (playbackType === "podcast" && currentPodcast?.progress?.currentTime) {
            time = currentPodcast.progress.currentTime;
        }
    }
    const displayTime = clampTime(time, duration);

    const progress = duration > 0
        ? Math.min(100, Math.max(0, (displayTime / duration) * 100))
        : 0;

    return { duration, displayTime, progress };
}
