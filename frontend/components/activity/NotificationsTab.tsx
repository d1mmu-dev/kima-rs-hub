"use client";

import {
    Bell,
    Check,
    Trash2,
    ListMusic,
    AlertCircle,
    CheckCircle,
    Clock,
    ExternalLink,
} from "lucide-react";
import { EmptyState } from "../ui/EmptyState";
import { GradientSpinner } from "../ui/GradientSpinner";
import { cn } from "@/lib/cn";
import Link from "next/link";
import { useNotifications, type Notification } from "@/hooks/useNotifications";
import { timeAgo } from "@/lib/relative-time";

export function NotificationsTab() {
    const {
        notifications,
        isLoading: loading,
        markAsRead: handleMarkAsRead,
        clearNotification: handleClear,
        clearAll: handleClearAll,
    } = useNotifications();

    const getIcon = (type: string) => {
        switch (type) {
            case "download_complete":
                return <CheckCircle className="w-4 h-4 text-success" />;
            case "download_failed":
            case "error":
                return <AlertCircle className="w-4 h-4 text-danger-light" />;
            case "download_waiting":
                return <Clock className="w-4 h-4 text-warning" />;
            case "playlist_ready":
            case "import_complete":
                return <ListMusic className="w-4 h-4 text-brand" />;
            case "system":
            default:
                return <Bell className="w-4 h-4 text-[var(--text-secondary)]" />;
        }
    };

    const getLink = (notification: Notification): string | null => {
        if (notification.metadata?.href) {
            return notification.metadata.href;
        }
        if (notification.metadata?.playlistId) {
            return `/playlist/?id=${encodeURIComponent(notification.metadata.playlistId)}`;
        }
        if (notification.metadata?.albumId) {
            return `/album/?id=${encodeURIComponent(notification.metadata.albumId)}`;
        }
        if (notification.metadata?.artistId) {
            return `/artist/?id=${encodeURIComponent(notification.metadata.artistId)}`;
        }
        return null;
    };

    if (loading) {
        return (
            <div className="flex items-center justify-center py-8">
                <GradientSpinner size="sm" />
            </div>
        );
    }

    if (notifications.length === 0) {
        return (
            <EmptyState
                icon={<Bell className="w-8 h-8" />}
                title="No notifications"
                description="You're all caught up!"
            />
        );
    }

    return (
        <div className="flex flex-col h-full">
            <div className="flex items-center justify-between px-3 py-2 border-b-2 border-white/10">
                <span className="text-xs tabular-nums font-semibold text-[var(--text-muted)]">
                    {String(notifications.length).padStart(2, "0")} ITEM
                    {notifications.length !== 1 ? "S" : ""}
                </span>
                <button
                    onClick={handleClearAll}
                    className="text-micro font-semibold text-[var(--text-muted)] hover:text-white transition-colors"
                >
                    CLEAR
                </button>
            </div>

            <div className="flex-1 overflow-y-auto">
                {notifications.map((notification, index) => {
                    const link = getLink(notification);

                    return (
                        <div
                            key={notification.id}
                            className={cn(
                                "px-3 py-3 border-b border-white/5 transition-colors group relative",
                                !notification.read && "bg-[var(--bg-secondary)] border-l-2 border-brand",
                                notification.read && "border-l-2 border-transparent hover:border-white/20 hover:bg-white/5"
                            )}
                        >
                            <div className="flex items-start gap-3">
                                <div className="flex-shrink-0 w-6 mt-0.5">
                                    <span className="text-micro tabular-nums font-semibold text-[var(--text-muted)]">
                                        {String(index + 1).padStart(2, "0")}
                                    </span>
                                </div>

                                <div className="mt-0.5 flex-shrink-0">
                                    {getIcon(notification.type)}
                                </div>

                                <div className="flex-1 min-w-0">
                                    <div className="flex items-center gap-2 mb-1">
                                        <p
                                            className={cn(
                                                "text-xs font-semibold tracking-tight truncate",
                                                notification.read
                                                    ? "text-[var(--text-muted)]"
                                                    : "text-white"
                                            )}
                                        >
                                            {notification.title}
                                        </p>
                                    </div>
                                    {notification.message && (
                                        <p className="text-micro text-[var(--text-muted)] mt-0.5 line-clamp-2 tabular-nums">
                                            {notification.message}
                                        </p>
                                    )}
                                    <div className="flex items-center gap-2 mt-1.5">
                                        <span className="text-xs tabular-nums text-[var(--text-muted)]">
                                            {timeAgo(notification.createdAt)}
                                        </span>
                                        {link && (
                                            <Link
                                                href={link}
                                                className="text-micro font-semibold text-brand hover:underline flex items-center gap-1"
                                            >
                                                VIEW
                                                <ExternalLink className="w-2.5 h-2.5" />
                                            </Link>
                                        )}
                                    </div>
                                </div>

                                <div className="flex items-center gap-1">
                                    {!notification.read && (
                                        <button
                                            onClick={() => handleMarkAsRead(notification.id)}
                                            className="p-1 hover:bg-white/10 transition-colors"
                                            title="mark as read"
                                            aria-label="Mark as read"
                                        >
                                            <Check className="w-3 h-3 text-[var(--text-muted)] hover:text-white" />
                                        </button>
                                    )}
                                    <button
                                        onClick={() => handleClear(notification.id)}
                                        className="p-1 hover:bg-white/10 transition-colors"
                                        title="dismiss"
                                        aria-label="Dismiss"
                                    >
                                        <Trash2 className="w-3 h-3 text-[var(--text-muted)] hover:text-danger-light" />
                                    </button>
                                </div>
                            </div>
                        </div>
                    );
                })}
            </div>
        </div>
    );
}
