"use client";

import type { ReactNode } from "react";
import type { Audiobook } from "../types";
import type { ColorPalette } from "@/hooks/useImageColor";
import { SpokenHero, type SpokenHeroDetail } from "@/features/spoken/SpokenHero";

interface AudiobookHeroProps {
    audiobook: Audiobook;
    heroImage: string | null;
    colors: ColorPalette | null;
    formatTime: (seconds: number) => string;
    children?: ReactNode;
}

export function AudiobookHero({
    audiobook,
    heroImage,
    colors,
    formatTime,
    children,
}: AudiobookHeroProps) {
    const progressPercent = audiobook.progress?.progress || 0;
    const isFinished = audiobook.progress?.isFinished;

    const details: SpokenHeroDetail[] = [
        ...(audiobook.narrator ? [{ text: audiobook.narrator }] : []),
        { text: formatTime(audiobook.duration) },
        {
            text: isFinished ? "Finished" : `${Math.round(progressPercent)}% complete`,
            className: isFinished ? "text-success" : "text-brand",
        },
    ];

    const tags = [
        ...(audiobook.series
            ? [{ label: `${audiobook.series.name} #${audiobook.series.sequence}`, accent: true }]
            : []),
        ...(audiobook.genres ?? []).slice(0, 3).map((label) => ({ label })),
    ];

    return (
        <SpokenHero
            kind="audiobook"
            title={audiobook.title}
            author={audiobook.author}
            description={audiobook.description}
            details={details}
            tags={tags}
            heroImage={heroImage}
            colors={colors}
        >
            {children}
        </SpokenHero>
    );
}
