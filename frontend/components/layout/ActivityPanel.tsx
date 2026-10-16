"use client";

import { useState, useEffect, useCallback } from "react";
import { useNotifications } from "@/hooks/useNotifications";
import { useActiveDownloads } from "@/hooks/useNotifications";
import { NotificationsTab } from "@/components/activity/NotificationsTab";
import { ActiveDownloadsTab } from "@/components/activity/ActiveDownloadsTab";
import { HistoryTab } from "@/components/activity/HistoryTab";
import { ImportsTab } from "@/components/activity/ImportsTab";
import {
    Bell,
    Download,
    History,
    ListMusic,
    X,
    type LucideIcon,
} from "lucide-react";
import { cn } from "@/lib/cn";
import { useIsMobile, useIsTablet } from "@/hooks/useMediaQuery";
import { useActivityPanelSettings } from "@/lib/activity-panel-settings-context";
import { useActivityPanelOpen } from "@/hooks/useActivityPanel";

type ActivityTab = "notifications" | "active" | "imports" | "history" | "settings";

const TABS: { id: ActivityTab; label: string; icon: LucideIcon }[] = [
    { id: "notifications", label: "Notifications", icon: Bell },
    { id: "active", label: "Active", icon: Download },
    { id: "imports", label: "Imports", icon: ListMusic },
    { id: "history", label: "History", icon: History },
];

interface ActivityPanelProps {
    isOpen: boolean;
    onToggle: () => void;
    activeTab?: ActivityTab;
    onTabChange?: (tab: ActivityTab) => void;
}

export function ActivityPanel({
    isOpen,
    onToggle,
    activeTab,
    onTabChange,
}: ActivityPanelProps) {
    const { settingsContent, setSettingsContent } = useActivityPanelSettings();
    const [internalActiveTab, setInternalActiveTab] =
        useState<ActivityTab>("notifications");
    const resolvedActiveTab = activeTab ?? internalActiveTab;
    const setResolvedActiveTab = onTabChange ?? setInternalActiveTab;
    const { unreadCount } = useNotifications();
    const { downloads: activeDownloads } = useActiveDownloads();

    const handleTabClick = useCallback((tab: ActivityTab) => {
        if (tab !== "settings" && resolvedActiveTab === "settings" && settingsContent) {
            setSettingsContent(null);
        }
        setResolvedActiveTab(tab);
    }, [resolvedActiveTab, settingsContent, setSettingsContent, setResolvedActiveTab]);

    // If settings tab is active but no settings content provided, default to notifications
    useEffect(() => {
        if (resolvedActiveTab === "settings" && !settingsContent) {
            setResolvedActiveTab("notifications");
        }
    }, [resolvedActiveTab, settingsContent, setResolvedActiveTab]);

    const notificationBadge = unreadCount > 0 ? unreadCount : null;
    const activeBadge =
        activeDownloads.length > 0 ? activeDownloads.length : null;

    // A phone or tablet overlay: the desktop layout shows the side panel (UnifiedPanel).
    if (!isOpen) return null;

    return (
        <>
            <div
                className="fixed inset-0 bg-black/60 z-(--z-panel-backdrop)"
                onClick={onToggle}
            />

            {/* Panel - slides in from right */}
            <div
                id="activity-panel-mobile"
                className="fixed inset-y-0 right-0 w-full max-w-md bg-[var(--bg-primary)] z-(--z-panel) flex flex-col"
                style={{ paddingTop: "var(--standalone-safe-area-top, 0px)" }}
            >
                <div className="flex items-center justify-between px-4 py-4 border-b border-white/10">
                    <h2 className="text-lg font-semibold text-white">
                        Activity
                    </h2>
                    <button
                        onClick={onToggle}
                        className="p-2 hover:bg-white/10 rounded-full transition-colors"
                        title="close"
                    >
                        <X className="w-5 h-5 text-[var(--text-secondary)]" />
                    </button>
                </div>

                <div className="flex border-b border-white/10">
                    {TABS.map((tab) => {
                        const Icon = tab.icon;
                        const badge =
                            tab.id === "notifications"
                                ? notificationBadge
                                : tab.id === "active"
                                ? activeBadge
                                : null;

                        return (
                            <button
                                key={tab.id}
                                onClick={() => handleTabClick(tab.id)}
                                className={cn(
                                    "flex-1 flex items-center justify-center gap-2 py-3 text-sm font-medium transition-colors duration-200 relative",
                                    resolvedActiveTab === tab.id
                                        ? "text-white border-b-2 border-[#f5c518]"
                                        : "text-[var(--text-secondary)] hover:text-white/70"
                                )}
                            >
                                <Icon className="w-4 h-4" />
                                <span>{tab.label}</span>
                                {badge && (
                                    <span
                                        className={cn(
                                            "min-w-[18px] h-[18px] px-1 rounded-full text-xs font-semibold flex items-center justify-center ml-1",
                                            tab.id === "active"
                                                ? "bg-brand text-black"
                                                : "bg-white/15 text-white"
                                        )}
                                    >
                                        {badge > 99 ? "99+" : badge}
                                    </span>
                                )}
                            </button>
                        );
                    })}
                </div>

                <div className="flex-1 overflow-hidden">
                    {resolvedActiveTab === "notifications" && (
                        <NotificationsTab />
                    )}
                    {resolvedActiveTab === "active" && (
                        <ActiveDownloadsTab />
                    )}
                    {resolvedActiveTab === "imports" && <ImportsTab />}
                    {resolvedActiveTab === "history" && <HistoryTab />}
                    {resolvedActiveTab === "settings" && settingsContent}
                </div>
            </div>
        </>
    );
}

export function ActivityPanelToggle() {
    const { unreadCount } = useNotifications();
    const { downloads: activeDownloads } = useActiveDownloads();
    const isMobile = useIsMobile();
    const isTablet = useIsTablet();
    const isPanelOpen = useActivityPanelOpen();

    if (isMobile || isTablet) {
        return null;
    }

    const hasActivity = unreadCount > 0 || activeDownloads.length > 0;

    return (
        <button
            onClick={() =>
                window.dispatchEvent(new CustomEvent("toggle-activity-panel"))
            }
            aria-expanded={isPanelOpen}
            aria-controls="activity-panel"
            className={cn(
                "relative p-2 rounded-full transition-all",
                "text-[var(--text-secondary)] hover:text-white"
            )}
            title="toggle activity panel"
            aria-label="Toggle activity panel"
        >
            <Bell className="w-5 h-5" />
            {hasActivity && (
                <span className="absolute top-1.5 right-2 w-1 h-1 rounded-full bg-brand" />
            )}
        </button>
    );
}
