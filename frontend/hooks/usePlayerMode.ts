import { useEffect } from "react";
import { useSearchParams } from "next/navigation";
import { usePathname } from "@/lib/use-pathname";
import { useAudio } from "@/lib/audio-context";

/** Full player on album, audiobook and podcast pages, mini elsewhere; the overlay is only opened by the user. */
export function usePlayerMode() {
    const pathname = usePathname();
    const searchParams = useSearchParams();
    const pageId = searchParams.get("id");
    const { currentTrack, currentAudiobook, currentPodcast, playerMode, setPlayerMode } = useAudio();

    useEffect(() => {
        // Don't auto-switch if in overlay mode (user manually opened it)
        if (playerMode === "overlay") return;

        if (!currentTrack && !currentAudiobook && !currentPodcast) return;

        // Determine if we're on the EXACT page where the current media is playing
        const isOnCurrentMediaPage =
            (currentTrack && pathname === "/album" && pageId === currentTrack.album?.id) ||
            (currentAudiobook && pathname === "/audiobooks/book" && pageId === currentAudiobook.id) ||
            (currentPodcast && pathname === "/podcasts/show" && pageId === currentPodcast.id.split(":")[0]);

        // Auto-expand to full when on the current media page
        // But don't auto-minimize - let users keep it expanded if they want
        if (isOnCurrentMediaPage && playerMode === "mini") {
            setPlayerMode("full");
        }
    }, [
        pathname,
        pageId,
        currentTrack,
        currentAudiobook,
        currentPodcast,
        playerMode,
        setPlayerMode,
    ]);
}
