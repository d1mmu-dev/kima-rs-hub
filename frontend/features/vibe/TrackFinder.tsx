import type { InputHTMLAttributes, ReactNode } from "react";
import { Search } from "lucide-react";
import { cn } from "@/lib/cn";
import type { FoundTrack, useTrackFinder } from "./useTrackFinder";

const NOTE = "px-1 text-xs text-white/60";

export function FinderNote({ className, children }: { className?: string; children: ReactNode }) {
    return <p className={cn(NOTE, className)} aria-live="polite">{children}</p>;
}

export function TrackSearchInput({ id, label, ...props }: { id: string; label: string } & InputHTMLAttributes<HTMLInputElement>) {
    return (
        <div className="relative">
            <label htmlFor={id} className="sr-only">{label}</label>
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-[var(--text-muted)]" />
            <input
                id={id}
                type="text"
                className="w-full pl-8 pr-3 py-2 bg-white/8 border border-white/10 rounded-lg text-sm text-white placeholder-white/55 focus:outline-none focus:border-white/20"
                {...props}
            />
        </div>
    );
}

interface TrackFinderResultsProps {
    finder: ReturnType<typeof useTrackFinder>;
    query: string;
    onPick: (track: FoundTrack) => void;
    className?: string;
}

export function TrackFinderResults({ finder, query, onPick, className }: TrackFinderResultsProps) {
    return (
        <>
            {finder.items.length > 0 && (
                <div className={cn("max-h-40 overflow-y-auto border border-white/10 rounded-lg", className)}>
                    {finder.items.map((track) => (
                        <button
                            key={track.id}
                            onClick={() => onPick(track)}
                            className="w-full px-3 py-2 hover:bg-white/10 text-left"
                        >
                            <p className="text-sm text-[var(--text-primary)] truncate">{track.title}</p>
                            <p className="text-xs text-white/60 truncate">
                                {track.artist.name}{track.via === "sound" ? " · sounds like it" : ""}
                            </p>
                        </button>
                    ))}
                </div>
            )}
            {finder.searching && <FinderNote className={className}>Searching…</FinderNote>}
            {finder.empty && (
                <FinderNote className={className}>
                    No track or artist matches &ldquo;{query.trim()}&rdquo;. Try part of a title, an artist, or a sound like &ldquo;piano&rdquo;.
                </FinderNote>
            )}
        </>
    );
}
