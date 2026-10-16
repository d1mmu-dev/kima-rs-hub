"use client";

import { useCallback } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuth } from "@/lib/auth-context";

export type Notification = Awaited<ReturnType<typeof api.getNotifications>>[number];

export interface DownloadHistoryItem {
    id: string;
    subject: string;
    type: string;
    targetMbid: string;
    status: string;
    error?: string;
    createdAt: string;
    updatedAt: string;
    completedAt?: string;
    metadata?: {
        statusText?: string;
        currentSource?: "lidarr" | "soulseek";
        lidarrAttempts?: number;
        soulseekAttempts?: number;
        [key: string]: unknown;
    };
}

const EMPTY_DOWNLOADS: DownloadHistoryItem[] = [];

const NOTIFICATIONS_KEY = ["notifications"];

// Applies `update` to the cached list before the request and restores the snapshot if it fails.
function useOptimisticNotifications<V>(
    mutationFn: (vars: V) => Promise<unknown>,
    update: (list: Notification[], vars: V) => Notification[],
) {
    const queryClient = useQueryClient();
    return useMutation({
        mutationFn,
        onMutate: async (vars: V) => {
            await queryClient.cancelQueries({ queryKey: NOTIFICATIONS_KEY });
            const previous = queryClient.getQueryData<Notification[]>(NOTIFICATIONS_KEY);
            queryClient.setQueryData<Notification[]>(NOTIFICATIONS_KEY, (old) => update(old ?? [], vars));
            return { previous };
        },
        onError: (_err, _vars, context) => {
            if (context?.previous) queryClient.setQueryData(NOTIFICATIONS_KEY, context.previous);
        },
    }).mutate;
}

/** Notifications from the shared query cache, with optimistic read and clear actions. */
export function useNotifications() {
    const { isAuthenticated } = useAuth();

    const { data: notifications = [], isLoading } = useQuery({
        queryKey: NOTIFICATIONS_KEY,
        queryFn: () => api.getNotifications(),
        enabled: isAuthenticated,
    });

    const markAsRead = useOptimisticNotifications(
        (id: string) => api.markNotificationAsRead(id),
        (list, id) => list.map((n) => (n.id === id ? { ...n, read: true } : n)),
    );
    const clearNotification = useOptimisticNotifications(
        (id: string) => api.clearNotification(id),
        (list, id) => list.filter((n) => n.id !== id),
    );
    const clearAll = useOptimisticNotifications(
        () => api.clearAllNotifications(),
        () => [],
    );

    return {
        notifications,
        unreadCount: notifications.filter((n) => !n.read).length,
        isLoading,
        markAsRead,
        clearNotification,
        clearAll: () => clearAll(undefined),
    };
}

/** Finished and failed downloads; SSE download events refresh it. */
export function useDownloadHistory() {
    const { isAuthenticated } = useAuth();
    const { data: history = EMPTY_DOWNLOADS, isLoading } = useQuery<DownloadHistoryItem[]>({
        queryKey: ["download-history"],
        queryFn: () => api.getDownloadHistory() as Promise<DownloadHistoryItem[]>,
        enabled: isAuthenticated,
        // Not playback-related, so it may refresh when the window regains focus.
        refetchOnWindowFocus: true,
    });
    return { history, isLoading };
}

/**
 * Hook for active downloads. Updates are driven by SSE events
 * (download:complete, download:failed) invalidating the query cache.
 */
export function useActiveDownloads() {
    const { isAuthenticated } = useAuth();
    const fetchDownloads = useCallback(async () => {
        return api.get<DownloadHistoryItem[]>("/notifications/downloads/active");
    }, []);

    const {
        data: downloads = EMPTY_DOWNLOADS,
        isLoading,
        error,
        refetch,
    } = useQuery<DownloadHistoryItem[]>({
        queryKey: ["active-downloads"],
        queryFn: fetchDownloads,
        enabled: isAuthenticated,
        // SSE drives updates; poll fast only while something is downloading.
        // The 30s idle tick bounds the cost of a missed SSE event.
        refetchInterval: (query) =>
            (query.state.data?.length ?? 0) > 0 ? 2000 : 30000,
        refetchIntervalInBackground: false, // Stop polling when tab is hidden
    });

    return {
        downloads,
        isLoading,
        error: error instanceof Error ? error.message : null,
        refetch,
    };
}
