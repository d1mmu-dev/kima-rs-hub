"use client";

import { useEffect, useRef, type CSSProperties } from "react";
import { Loader2 } from "lucide-react";
import { useLyricsSync } from "@/hooks/useLyricsSync";
import { cn } from "@/lib/cn";

const fade = (edge: number): CSSProperties => {
    const mask = `linear-gradient(to bottom, transparent 0%, black ${edge}%, black ${100 - edge}%, transparent 100%)`;
    return { maskImage: mask, WebkitMaskImage: mask };
};

const VARIANTS = {
    // The vibe side panel: full height, larger type.
    panel: {
        frame: "flex-1",
        scroller: "flex-1 overflow-y-auto p-5",
        list: "space-y-1 py-8 overflow-hidden",
        line: "px-2 text-base",
        empty: "text-sm",
        active: "scale-110 opacity-100",
        idle: "scale-100 opacity-30",
        plain: "opacity-70",
        syncedMask: undefined,
        plainMask: undefined,
    },
    // The square that replaces cover art in the phone player: faded edges keep the active line centred.
    compact: {
        frame: "w-full h-full",
        scroller: "w-full h-full overflow-y-auto px-5 scrollbar-hide",
        list: "space-y-1 py-[40%]",
        line: "px-1 text-sm",
        empty: "text-xs",
        active: "scale-105 opacity-100",
        idle: "scale-100 opacity-25",
        plain: "opacity-60",
        syncedMask: fade(12),
        plainMask: fade(8),
    },
} as const;

export function LyricsView({ variant }: { variant: keyof typeof VARIANTS }) {
    const v = VARIANTS[variant];
    const { lines, activeIndex, isLoading, hasLyrics, isSynced, plainLyrics } = useLyricsSync();
    const activeRef = useRef<HTMLDivElement>(null);
    const scrollerRef = useRef<HTMLDivElement>(null);

    // Scroll only the lyrics box: scrollIntoView would also scroll every ancestor, the app layout included.
    useEffect(() => {
        const box = scrollerRef.current;
        const line = activeRef.current;
        if (!box || !line) return;
        const offset = line.getBoundingClientRect().top - box.getBoundingClientRect().top;
        box.scrollTo({ top: box.scrollTop + offset - box.clientHeight / 2 + line.offsetHeight / 2, behavior: "smooth" });
    }, [activeIndex]);

    if (isLoading) {
        return (
            <div className={cn(v.frame, "flex items-center justify-center")}>
                <Loader2 className="w-6 h-6 animate-spin text-[var(--text-muted)]" />
            </div>
        );
    }

    if (!hasLyrics) {
        return (
            <div className={cn(v.frame, "flex items-center justify-center")}>
                <p className={cn(v.empty, "text-[var(--text-muted)]")}>No lyrics available</p>
            </div>
        );
    }

    return (
        <div ref={scrollerRef} className={v.scroller} style={isSynced ? v.syncedMask : v.plainMask}>
            <div className={v.list}>
                {isSynced
                    ? lines.map((line, i) => (
                          <div
                              key={`${line.time}-${i}`}
                              ref={i === activeIndex ? activeRef : undefined}
                              className={cn(
                                  "py-1.5 rounded text-white origin-left transition-[transform,opacity] duration-300",
                                  v.line,
                                  i === activeIndex ? v.active : v.idle,
                              )}
                          >
                              {line.text}
                          </div>
                      ))
                    : plainLyrics?.split("\n").map((line, i) => (
                          <div key={i} className={cn("py-1.5 rounded text-white", v.line, v.plain)}>
                              {line || " "}
                          </div>
                      ))}
            </div>
        </div>
    );
}
