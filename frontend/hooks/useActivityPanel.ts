"use client";

import { useState, useEffect, useCallback, useMemo, useRef } from "react";

const ACTIVITY_PANEL_KEY = "kima_activity_panel_open";

export type ActivityPanelTab = "notifications" | "active" | "imports" | "history" | "settings";

export function setActivityPanelTab(tab: ActivityPanelTab) {
    window.dispatchEvent(new CustomEvent("set-activity-panel-tab", { detail: { tab } }));
}

/** Opens the activity panel, on `tab` when one is given. */
export function openActivityPanel(tab?: ActivityPanelTab) {
    if (tab) setActivityPanelTab(tab);
    window.dispatchEvent(new CustomEvent("open-activity-panel"));
}

/** Follows the panel's open state from the window events, for controls that only show it. */
export function useActivityPanelOpen(): boolean {
    const [isOpen, setIsOpen] = useState(() => {
        if (typeof window === "undefined") return false;
        return localStorage.getItem(ACTIVITY_PANEL_KEY) === "true";
    });

    useEffect(() => {
        const handleToggle = () => setIsOpen((prev) => !prev);
        const handleOpen = () => setIsOpen(true);
        const handleClose = () => setIsOpen(false);
        window.addEventListener("toggle-activity-panel", handleToggle);
        window.addEventListener("open-activity-panel", handleOpen);
        window.addEventListener("close-activity-panel", handleClose);
        return () => {
            window.removeEventListener("toggle-activity-panel", handleToggle);
            window.removeEventListener("open-activity-panel", handleOpen);
            window.removeEventListener("close-activity-panel", handleClose);
        };
    }, []);

    return isOpen;
}

export function useActivityPanel() {
    const [isOpen, setIsOpen] = useState(() => {
        if (typeof window === "undefined") return false;
        return localStorage.getItem(ACTIVITY_PANEL_KEY) === "true";
    });
    const [activeTab, setActiveTab] = useState<ActivityPanelTab>("notifications");

    useEffect(() => {
        if (typeof window !== "undefined") {
            localStorage.setItem(ACTIVITY_PANEL_KEY, isOpen ? "true" : "false");
        }
    }, [isOpen]);

    // The actions announce themselves on the window so TopBar and ActivityPanel
    // keep aria-expanded in step, and AuthenticatedLayout reacts to that event.
    // The announcement must stay outside any state updater; reading the current
    // value from this ref, plus the early return in open()/close(), prevents re-entry.
    const isOpenRef = useRef(isOpen);
    useEffect(() => {
        isOpenRef.current = isOpen;
    }, [isOpen]);

    const announce = (next: boolean) => {
        window.dispatchEvent(
            new CustomEvent(next ? "open-activity-panel" : "close-activity-panel")
        );
    };

    const toggle = useCallback(() => {
        const next = !isOpenRef.current;
        isOpenRef.current = next;
        setIsOpen(next);
        announce(next);
    }, []);

    const open = useCallback(() => {
        if (isOpenRef.current) return;
        isOpenRef.current = true;
        setIsOpen(true);
        announce(true);
    }, []);

    const close = useCallback(() => {
        if (!isOpenRef.current) return;
        isOpenRef.current = false;
        setIsOpen(false);
        announce(false);
    }, []);

    return useMemo(() => ({
        isOpen,
        activeTab,
        setActiveTab,
        toggle,
        open,
        close,
    }), [isOpen, activeTab, setActiveTab, toggle, open, close]);
}
