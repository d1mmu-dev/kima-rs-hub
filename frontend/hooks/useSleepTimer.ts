"use client";

import { useEffect, useCallback, useRef, useSyncExternalStore } from "react";
import { useAudioControls } from "@/lib/audio-controls-context";

// Module-level shared state so timer persists across player mode switches
let endTime: number | null = null;
let remaining: number | null = null;
let tickInterval: ReturnType<typeof setInterval> | null = null;
let expireCallback: (() => void) | null = null;

const listeners = new Set<() => void>();
function notify() { listeners.forEach((fn) => fn()); }

function subscribe(cb: () => void) {
    listeners.add(cb);
    return () => { listeners.delete(cb); };
}

function tick() {
    if (endTime === null) {
        remaining = null;
        return;
    }
    const leftMs = Math.max(0, endTime - Date.now());
    if (leftMs <= 0) {
        endTime = null;
        remaining = null;
        if (tickInterval) { clearInterval(tickInterval); tickInterval = null; }
        expireCallback?.();
        notify();
        return;
    }
    remaining = Math.ceil(leftMs / 1000);
    notify();
}

function startTimer(minutes: number) {
    endTime = Date.now() + minutes * 60 * 1000;
    tick();
    if (!tickInterval) tickInterval = setInterval(tick, 1000);
}

function stopTimer() {
    endTime = null;
    remaining = null;
    if (tickInterval) { clearInterval(tickInterval); tickInterval = null; }
    notify();
}

function getEndTime() { return endTime; }
function getRemaining() { return remaining; }
function getServerSnapshot() { return null; }

interface SleepTimerState {
    remainingSeconds: number | null;
    isActive: boolean;
    setTimer: (minutes: number) => void;
    clearTimer: () => void;
    displayRemaining: string;
}

export function useSleepTimer(): SleepTimerState {
    const { pause } = useAudioControls();

    const pauseRef = useRef(pause);
    useEffect(() => { pauseRef.current = pause; }, [pause]);

    useEffect(() => {
        expireCallback = () => pauseRef.current();
        return () => { expireCallback = null; };
    }, []);

    const active = useSyncExternalStore(subscribe, getEndTime, getServerSnapshot);
    const remainingSeconds = useSyncExternalStore(subscribe, getRemaining, getServerSnapshot);

    const setTimer = useCallback((minutes: number) => { startTimer(minutes); }, []);
    const clearTimer = useCallback(() => { stopTimer(); }, []);

    let displayRemaining = "";
    if (remainingSeconds !== null) {
        if (remainingSeconds >= 3600) {
            const h = Math.floor(remainingSeconds / 3600);
            const m = Math.ceil((remainingSeconds % 3600) / 60);
            displayRemaining = `${h}h ${m}m`;
        } else if (remainingSeconds >= 60) {
            displayRemaining = `${Math.ceil(remainingSeconds / 60)}m`;
        } else {
            displayRemaining = `0:${String(remainingSeconds).padStart(2, "0")}`;
        }
    }

    return {
        remainingSeconds,
        isActive: active !== null,
        setTimer,
        clearTimer,
        displayRemaining,
    };
}
