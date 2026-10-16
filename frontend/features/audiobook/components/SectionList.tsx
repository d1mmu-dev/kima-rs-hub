"use client";

import type { AudiobookSection } from "../types";
import { SectionHeader } from "@/components/ui/SectionHeader";

interface SectionListProps {
    sections: AudiobookSection[];
    onSeekToSection: (startTime: number) => void;
    formatTime: (seconds: number) => string;
}

export function SectionList({
    sections,
    onSeekToSection,
    formatTime,
}: SectionListProps) {
    if (!sections || sections.length === 0 || sections.length > 50) {
        return null;
    }

    return (
        <section>
            <SectionHeader type="audiobook" title="Chapters" count={sections.length} />

            <div className="rounded-lg border border-white/10 bg-[var(--bg-primary)] overflow-hidden">
                <div className="divide-y divide-white/5">
                    {sections.map((section, index) => (
                        <button
                            key={section.index}
                            onClick={() => onSeekToSection(section.start)}
                            className="w-full text-left px-4 py-3 hover:bg-white/[0.03] transition-colors group flex items-center gap-4"
                        >
                            <span className="text-xs tabular-nums text-[var(--text-muted)] w-6 text-right shrink-0">
                                {index + 1}
                            </span>
                            <span className="text-sm font-bold text-white group-hover:text-brand transition-colors truncate tracking-tight">
                                {section.title}
                            </span>
                            <span className="ml-auto text-xs tabular-nums text-[var(--text-muted)] shrink-0">
                                {formatTime(section.start)}
                            </span>
                        </button>
                    ))}
                </div>
            </div>
        </section>
    );
}
