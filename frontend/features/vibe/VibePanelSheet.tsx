"use client";

import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { motion, useDragControls, type PanInfo } from "framer-motion";
import { ActivityIconBar, type ActivityType } from "./ActivityIconBar";
import {
    type VibeTab,
    ActivityContent,
    ActivityHeader,
    TabBar,
    TabContent,
} from "./panel-shared";
import { useMediaQuery } from "@/hooks/useMediaQuery";
import Link from "next/link";
import { Waypoints, Sparkles, AudioWaveform, Play, type LucideIcon } from "lucide-react";
import { cn } from "@/lib/cn";

type SnapPoint = "peek" | "half" | "full";

/** The closed sheet's handle. */
const SHEET_PEEK = 28;

function SheetAction({ icon: Icon, label, primary = false, ...props }: { icon: LucideIcon; label: string; primary?: boolean } & React.ButtonHTMLAttributes<HTMLButtonElement>) {
    return (
        <button
            className={cn(
                "flex-1 flex flex-col items-center gap-1 min-h-[52px] py-2 px-1 rounded-lg landscape-short:flex-row landscape-short:justify-center landscape-short:gap-1.5 landscape-short:min-h-11 landscape-short:py-1 border text-[var(--text-primary)] transition-colors focus-visible:outline-none focus-visible:ring-2",
                primary
                    ? "bg-[var(--color-brand)]/15 border-[var(--color-brand)]/40 focus-visible:ring-[var(--color-brand)]/60"
                    : "bg-white/[0.06] hover:bg-white/[0.12] border-white/10 focus-visible:ring-white/40",
            )}
            {...props}
        >
            <Icon className={cn("w-4 h-4 shrink-0", primary && "fill-current")} />
            <span className="text-micro font-medium leading-tight text-center">{label}</span>
        </button>
    );
}

export interface VibePanelSheetProps {
    selectedTrackId?: string | null;
    selectedTrackTitle?: string | null;
    selectedTrackArtist?: string | null;
    onTrackOperation?: (op: "vibe" | "similar") => void;
    onStartSongPath?: () => void;
    songPathActive?: boolean;
    onPlay?: () => void;
    selectedArtistId?: string | null;
    /** Leave the space the floating mini player covers (--mini-clearance). */
    aboveMiniPlayer?: boolean;
    /** First-visit help, shown under the handle while no track is selected. */
    intro?: React.ReactNode;
}

export function VibePanelSheet({
    selectedTrackId,
    selectedTrackTitle,
    selectedTrackArtist,
    onTrackOperation,
    onStartSongPath,
    songPathActive = false,
    aboveMiniPlayer = false,
    onPlay,
    selectedArtistId,
    intro,
}: VibePanelSheetProps) {
    const [snap, setSnap] = useState<SnapPoint>("peek");
    const [activeTab, setActiveTab] = useState<VibeTab>("now-playing");
    const [expandedActivity, setExpandedActivity] = useState<ActivityType | null>(null);
    const [sheetHeight, setSheetHeight] = useState(600);
    const prefersReducedMotion = useMediaQuery("(prefers-reduced-motion: reduce)");
    const dragControls = useDragControls();

    const sheetRef = useRef<HTMLDivElement>(null);
    // Size against the area above the mini player, which the sheet's frame already is.
    useEffect(() => {
        const update = () => {
            const host = sheetRef.current?.offsetParent as HTMLElement | null;
            const available = host?.clientHeight ?? window.innerHeight;
            setSheetHeight(Math.max(SHEET_PEEK * 6, available * 0.94));
        };
        update();
        window.addEventListener("resize", update);
        return () => window.removeEventListener("resize", update);
    }, [aboveMiniPlayer]);

    // The closed sheet shows, under the handle, the selected track's actions or the
    // first-visit help, at their measured height.
    const [peekContentHeight, setPeekContentHeight] = useState(0);
    const hasActions = !!(selectedTrackId && onTrackOperation && onStartSongPath);
    const hasPeekContent = hasActions || !!intro;
    const peekContentRef = useCallback((el: HTMLDivElement | null) => {
        if (!el) return;
        const observer = new ResizeObserver(() => setPeekContentHeight(el.offsetHeight));
        observer.observe(el);
        return () => observer.disconnect();
    }, []);
    const peekHeight = SHEET_PEEK + (hasPeekContent ? peekContentHeight : 0);
    const snapY = useMemo(() => ({
        peek: sheetHeight - peekHeight,
        half: sheetHeight * 0.5,
        full: 0,
    }), [sheetHeight, peekHeight]);

    const handleDragEnd = useCallback((_: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
        const currentY = snapY[snap] + info.offset.y;
        const vy = info.velocity.y;

        if (vy > 500) {
            setSnap(snap === "full" ? "half" : "peek");
        } else if (vy < -500) {
            setSnap(snap === "peek" ? "half" : "full");
        } else {
            const entries = Object.entries(snapY) as [SnapPoint, number][];
            entries.sort((a, b) => Math.abs(a[1] - currentY) - Math.abs(b[1] - currentY));
            setSnap(entries[0][0]);
        }
    }, [snap, snapY]);

    const handleToggleActivity = (type: ActivityType) => {
        setExpandedActivity((prev) => (prev === type ? null : type));
        if (snap === "peek") setSnap("half");
    };

    const handleTabClick = (tab: VibeTab) => {
        setActiveTab(tab);
        setExpandedActivity(null);
        if (snap === "peek") setSnap("half");
    };

    return (
        <div className="pointer-events-none absolute inset-x-0 top-0 z-(--z-scene-panel) overflow-hidden" style={{ bottom: aboveMiniPlayer ? "var(--mini-clearance)" : 0 }}>
        <motion.div
            ref={sheetRef}
            className="pointer-events-auto absolute bottom-0 left-0 right-0 bg-[var(--bg-primary)] rounded-t-2xl border-t border-white/10 flex flex-col"
            style={{ height: sheetHeight }}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1, y: snapY[snap] }}
            transition={prefersReducedMotion
                ? { duration: 0 }
                : { type: "spring", damping: 30, stiffness: 300 }
            }
            drag="y"
            dragControls={dragControls}
            dragListener={false}
            dragConstraints={{ top: 0, bottom: snapY.peek }}
            dragElastic={0.2}
            onDragEnd={handleDragEnd}
        >
            <div
                className="flex justify-center py-3 cursor-grab active:cursor-grabbing touch-none shrink-0"
                onPointerDown={(e) => dragControls.start(e)}
            >
                <div className="w-10 h-1 bg-white/20 rounded-full" />
            </div>

            {!hasActions && intro && (
                <div ref={peekContentRef} className="shrink-0 px-4 pb-3 border-b border-white/5">{intro}</div>
            )}

            {hasActions && onTrackOperation && onStartSongPath && (
                <div ref={peekContentRef} className="shrink-0 px-3 pb-2 border-b border-white/5 landscape-short:flex landscape-short:items-center landscape-short:gap-3">
                    {(selectedTrackTitle || selectedTrackArtist) && (
                        <p className="text-xs text-[var(--text-muted)] truncate mb-2 mt-0.5 landscape-short:m-0 landscape-short:w-44 landscape-short:shrink-0">
                            <span className="text-[var(--text-secondary)]">{selectedTrackTitle}</span>
                            {selectedTrackArtist && (
                                <> &middot; {selectedArtistId
                                    ? <Link href={`/artist/?id=${encodeURIComponent(selectedArtistId)}`} className="underline-offset-2 hover:underline">{selectedTrackArtist}</Link>
                                    : selectedTrackArtist}</>
                            )}
                        </p>
                    )}
                    <div className="flex gap-2 landscape-short:min-w-0 landscape-short:flex-1">
                        {onPlay && <SheetAction primary icon={Play} label="Play" onClick={onPlay} aria-label="Play this track" />}
                        <SheetAction
                            icon={Sparkles}
                            label="Match Vibe"
                            onClick={() => onTrackOperation("vibe")}
                            aria-label="Match Vibe -- build a queue of similar-sounding tracks"
                        />
                        <SheetAction
                            icon={AudioWaveform}
                            label="Similar"
                            onClick={() => onTrackOperation("similar")}
                            aria-label="Similar -- highlight similar tracks on the map"
                        />
                        <SheetAction
                            icon={Waypoints}
                            label="Song Path"
                            onClick={onStartSongPath}
                            aria-label="Song Path -- set this track as the start of a song path, then tap the destination"
                        />
                    </div>
                    {songPathActive && (
                        <p className="text-micro text-[var(--text-secondary)] text-center mt-2 animate-pulse">
                            Tap another track to complete the song path
                        </p>
                    )}
                </div>
            )}

            <div className="shrink-0">
                <ActivityIconBar
                    expandedActivity={expandedActivity}
                    onToggleActivity={handleToggleActivity}
                />
            </div>

            {expandedActivity ? (
                <>
                    <ActivityHeader
                        type={expandedActivity}
                        onClose={() => setExpandedActivity(null)}
                    />
                    <div className="flex-1 overflow-y-auto overscroll-contain">
                        <ActivityContent type={expandedActivity} />
                    </div>
                </>
            ) : (
                <>
                    <TabBar activeTab={activeTab} onTabClick={handleTabClick} />
                    <TabContent activeTab={activeTab} />
                </>
            )}
        </motion.div>
        </div>
    );
}
