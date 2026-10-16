"use client";

import {
    createContext,
    useContext,
    useState,
    ReactNode,
    useEffect,
    useMemo,
    useCallback,
    useRef,
    useLayoutEffect,
} from "react";
import { useActiveDownloads, DownloadHistoryItem } from "@/hooks/useNotifications";
import { useEventSource } from "@/hooks/useEventSource";

interface PendingDownload {
    id: string;
    type: "artist" | "album";
    subject: string;
    mbid: string; // Unique identifier for deduplication
    timestamp: number;
}

interface DownloadContextType {
    pendingDownloads: PendingDownload[];
    downloadStatus: {
        activeDownloads: DownloadHistoryItem[];
        hasActiveDownloads: boolean;
    };
    addPendingDownload: (
        type: "artist" | "album",
        subject: string,
        mbid: string
    ) => string | null;
    isPendingByMbid: (mbid: string) => boolean;
}

const DownloadContext = createContext<DownloadContextType | undefined>(
    undefined
);

export function DownloadProvider({ children }: { children: ReactNode }) {
    const [pendingDownloads, setPendingDownloads] = useState<PendingDownload[]>(
        []
    );
    const pendingDownloadsRef = useRef<PendingDownload[]>([]);
    useLayoutEffect(() => {
        pendingDownloadsRef.current = pendingDownloads;
    }, [pendingDownloads]);
    useEventSource();
    const { downloads: activeDownloads } = useActiveDownloads();

    const downloadStatus = useMemo(() => ({
        activeDownloads: activeDownloads.filter((d: DownloadHistoryItem) => d.status === "pending" || d.status === "processing"),
        hasActiveDownloads: activeDownloads.some((d: DownloadHistoryItem) => d.status === "pending" || d.status === "processing"),
    }), [activeDownloads]);

    // A pending entry is dropped once its job shows up among the active downloads.
    const [prevActiveDownloads, setPrevActiveDownloads] = useState(downloadStatus.activeDownloads);
    if (prevActiveDownloads !== downloadStatus.activeDownloads) {
        setPrevActiveDownloads(downloadStatus.activeDownloads);
        setPendingDownloads((prev) => {
            const next = prev.filter((pending) => {
                return !downloadStatus.activeDownloads.some(
                    (job) => job.targetMbid === pending.mbid
                );
            });
            return next.length === prev.length ? prev : next;
        });
    }

    // A job that fails at once never appears in any response, so entries expire after 2 minutes.
    useEffect(() => {
        const STALE_THRESHOLD = 2 * 60 * 1000;

        const cleanup = setInterval(() => {
            setPendingDownloads((prev) => {
                const now = Date.now();
                const next = prev.filter((pending) => now - pending.timestamp <= STALE_THRESHOLD);
                return next.length === prev.length ? prev : next;
            });
        }, 30000);

        return () => clearInterval(cleanup);
    }, []);

    const addPendingDownload = useCallback((
        type: "artist" | "album",
        subject: string,
        mbid: string
    ): string | null => {
        // The ref, written here before the state commits, catches a second click in the same tick.
        if (pendingDownloadsRef.current.some((d) => d.mbid === mbid)) {
            return null;
        }

        const id = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
        const download: PendingDownload = {
            id,
            type,
            subject,
            mbid,
            timestamp: Date.now(),
        };

        pendingDownloadsRef.current = [...pendingDownloadsRef.current, download];
        setPendingDownloads((prev) => (prev.some((d) => d.mbid === mbid) ? prev : [...prev, download]));

        return id;
    }, []);

    const isPendingByMbid = useCallback((mbid: string): boolean => {
        const isPendingLocal = pendingDownloads.some((d) => d.mbid === mbid);
        const hasActiveJob = downloadStatus.activeDownloads.some(
            (job) => job.targetMbid === mbid
        );

        return isPendingLocal || hasActiveJob;
    }, [pendingDownloads, downloadStatus.activeDownloads]);

    const contextValue = useMemo(() => ({
        pendingDownloads,
        downloadStatus,
        addPendingDownload,
        isPendingByMbid,
    }), [
        pendingDownloads,
        downloadStatus,
        addPendingDownload,
        isPendingByMbid,
    ]);

    return (
        <DownloadContext.Provider value={contextValue}>
            {children}
        </DownloadContext.Provider>
    );
}

export function useDownloadContext() {
    const context = useContext(DownloadContext);
    if (!context) {
        throw new Error(
            "useDownloadContext must be used within DownloadProvider"
        );
    }
    return context;
}
