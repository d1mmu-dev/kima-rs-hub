"use client";

import { ArrowLeft } from "lucide-react";
import { LyricsView } from "./LyricsView";

export function LyricsPanel({ onBack }: { onBack: () => void }) {
    return (
        <div className="h-full flex flex-col bg-[var(--bg-primary)]">
            <div className="flex items-center gap-3 px-5 py-4 border-b-2 border-white/20">
                <button
                    onClick={onBack}
                    className="border border-white/20 p-2 hover:border-brand hover:bg-white/5 transition-colors"
                    title="back to activity"
                    aria-label="Back to activity"
                >
                    <ArrowLeft className="w-4 h-4 text-[var(--text-secondary)]" />
                </button>
                <h3 className="text-sm font-bold text-white">Lyrics</h3>
            </div>
            <LyricsView variant="panel" />
        </div>
    );
}
