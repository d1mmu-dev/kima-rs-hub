"use client";

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AudioWaveform, Play, Sparkles, Waypoints } from "lucide-react";
import { normalizeNFC } from "@/lib/format";
import type { MapTrack } from "./types";

interface TrackMenuProps {
    track: MapTrack;
    at: { x: number; y: number };
    /** Pixels at the right edge the menu must not cover (an open side panel). */
    avoidRight?: number;
    onClose: () => void;
    onPlay: () => void;
    onVibe: () => void;
    onSimilar: () => void;
    onSongPath: () => void;
}

const ITEM = "flex w-full items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left text-xs text-[var(--text-primary)] transition-colors hover:bg-white/[0.08] focus-visible:bg-white/[0.08] focus-visible:outline-none";

/** Right-click on a star: the same four actions as the track panel, at the pointer. */
export function TrackMenu({ track, at, avoidRight = 0, onClose, onPlay, onVibe, onSimilar, onSongPath }: TrackMenuProps) {
    const ref = useRef<HTMLDivElement>(null);
    const [pos, setPos] = useState(at);

    // Keep the menu on screen near the viewport edges.
    useLayoutEffect(() => {
        const el = ref.current;
        if (!el) return;
        const r = el.getBoundingClientRect();
        setPos({
            x: Math.max(8, Math.min(at.x, window.innerWidth - avoidRight - r.width - 8)),
            y: Math.min(at.y, window.innerHeight - r.height - 8),
        });
        el.querySelector<HTMLButtonElement>("button")?.focus();
    }, [at, avoidRight]);

    useEffect(() => {
        const onDown = (e: PointerEvent) => {
            if (!ref.current?.contains(e.target as Node)) onClose();
        };
        document.addEventListener("pointerdown", onDown, true);
        window.addEventListener("wheel", onClose, { passive: true });
        return () => {
            document.removeEventListener("pointerdown", onDown, true);
            window.removeEventListener("wheel", onClose);
        };
    }, [onClose]);

    const run = (fn: () => void) => () => { onClose(); fn(); };

    // Portalled: the page wrapper animates with a transform, which would make
    // `fixed` resolve against it instead of the viewport.
    return createPortal(
        <div
            ref={ref}
            role="menu"
            aria-label={`Actions for ${track.title}`}
            className="vibe-glass vibe-glass--solid fixed z-(--z-dropdown) w-56 rounded-xl p-1.5 animate-fade-in"
            style={{ left: pos.x, top: pos.y }}
        >
            <p className="truncate px-2.5 pb-1.5 pt-1 text-[11px] text-[var(--text-muted)]">
                {normalizeNFC(track.title)} · {normalizeNFC(track.artist)}
            </p>
            <button role="menuitem" type="button" className={ITEM} onClick={run(onPlay)}><Play className="h-3.5 w-3.5" /> Play</button>
            <button role="menuitem" type="button" className={ITEM} onClick={run(onVibe)}><Sparkles className="h-3.5 w-3.5" /> Match Vibe</button>
            <button role="menuitem" type="button" className={ITEM} onClick={run(onSimilar)}><AudioWaveform className="h-3.5 w-3.5" /> Show similar</button>
            <button role="menuitem" type="button" className={ITEM} onClick={run(onSongPath)}><Waypoints className="h-3.5 w-3.5" /> Song Path from here</button>
        </div>,
        document.body,
    );
}
