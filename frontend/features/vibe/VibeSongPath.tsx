"use client";

import { useState, useCallback } from "react";
import { X, Route, ArrowRight } from "lucide-react";
import { Button } from "@/components/ui/Button";
import type { LibraryIndex } from "./librarySearch";
import { useTrackFinder } from "./useTrackFinder";
import { FinderNote, TrackFinderResults, TrackSearchInput } from "./TrackFinder";

interface VibeSongPathProps {
    library: LibraryIndex;
    onStartPath: (startId: string, endId: string) => void;
    onClose: () => void;
}

export function VibeSongPath({ library, onStartPath, onClose }: VibeSongPathProps) {
    const [startQuery, setStartQuery] = useState("");
    const [endQuery, setEndQuery] = useState("");
    const [startTrackId, setStartTrackId] = useState<string | null>(null);
    const [endTrackId, setEndTrackId] = useState<string | null>(null);
    const [activeInput, setActiveInput] = useState<"start" | "end" | null>(null);

    const activeQuery = activeInput === "start" ? startQuery : activeInput === "end" ? endQuery : "";
    const pickedAlready = activeInput === "start" ? !!startTrackId : activeInput === "end" ? !!endTrackId : true;
    const finder = useTrackFinder(library, activeQuery, !!activeInput && !pickedAlready);

    const selectTrack = useCallback((track: { id: string; title: string; artist: { name: string } }) => {
        const label = `${track.title} - ${track.artist.name}`;
        if (activeInput === "start") {
            setStartTrackId(track.id);
            setStartQuery(label);
            setActiveInput("end");
        } else {
            setEndTrackId(track.id);
            setEndQuery(label);
            setActiveInput(null);
        }
    }, [activeInput]);

    const sameTrack = !!startTrackId && startTrackId === endTrackId;
    const handleSubmit = useCallback(() => {
        if (startTrackId && endTrackId && startTrackId !== endTrackId) {
            onStartPath(startTrackId, endTrackId);
        }
    }, [startTrackId, endTrackId, onStartPath]);

    return (
        <div className="absolute top-16 left-1/2 -translate-x-1/2 z-(--z-scene-panel) w-[calc(100%-1.5rem)] sm:w-96 max-h-[calc(100%-5rem)] overflow-y-auto overscroll-contain border border-white/10 bg-[rgb(12,8,20)] shadow-[0_12px_32px_-12px_rgba(0,0,0,0.7)] rounded-2xl p-4">
            <div className="flex items-center justify-between mb-3">
                <h3 className="text-sm font-medium text-[var(--text-primary)] flex items-center gap-2">
                    <Route className="w-4 h-4" /> Song Path
                </h3>
                <button onClick={onClose} className="text-[var(--text-muted)] hover:text-white" aria-label="Close song path">
                    <X className="w-4 h-4" />
                </button>
            </div>

            <div className="space-y-2">
                <TrackSearchInput
                    id="path-start"
                    label="Start track"
                    value={startQuery}
                    onChange={(e) => { setStartQuery(e.target.value); setStartTrackId(null); }}
                    onFocus={() => setActiveInput("start")}
                    placeholder="Start: a track, artist or sound"
                />

                <div className="flex justify-center" aria-hidden="true">
                    <ArrowRight className="w-4 h-4 text-[var(--text-muted)]" />
                </div>

                <TrackSearchInput
                    id="path-end"
                    label="End track"
                    value={endQuery}
                    onChange={(e) => { setEndQuery(e.target.value); setEndTrackId(null); }}
                    onFocus={() => setActiveInput("end")}
                    placeholder="End: a track, artist or sound"
                />
            </div>

            <TrackFinderResults finder={finder} query={activeQuery} onPick={selectTrack} className="mt-2" />

            {sameTrack && <FinderNote className="mt-2">Pick a different track for the end.</FinderNote>}
            <Button className="w-full mt-3" onClick={handleSubmit} disabled={!startTrackId || !endTrackId || sameTrack}>
                <Route className="w-4 h-4" /> Generate Path
            </Button>
        </div>
    );
}
