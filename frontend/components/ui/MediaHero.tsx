"use client";

import { memo, type ReactNode } from "react";
import { HeroBackdrop } from "@/components/ui/HeroBackdrop";
import { CachedImage } from "@/components/ui/CachedImage";
import { cn } from "@/lib/cn";
import type { ColorPalette } from "@/hooks/useImageColor";

interface HeroCoverProps {
    image: string | null;
    alt: string;
    shape: "square" | "circle";
    fallbackIcon: ReactNode;
    bgClass?: string;
}

/** The 140/192 cover box under a hero: one place the size, ring and shadow live. */
export const HeroCover = memo(function HeroCover({
    image,
    alt,
    shape,
    fallbackIcon,
    bgClass = "bg-[#282828]",
}: HeroCoverProps) {
    return (
        <div
            className={cn(
                "w-[140px] h-[140px] md:w-[192px] md:h-[192px] shrink-0 overflow-hidden relative ring-1 ring-white/10 shadow-[0_8px_30px_rgba(0,0,0,0.45)]",
                shape === "circle" ? "rounded-full" : "rounded-[14px]",
                bgClass,
            )}
        >
            {image ? (
                <CachedImage
                    src={image}
                    alt={alt}
                    fill
                    sizes="(max-width: 768px) 140px, 192px"
                    className="object-cover"
                    priority
                />
            ) : (
                <div className="w-full h-full flex items-center justify-center">
                    {fallbackIcon}
                </div>
            )}
        </div>
    );
});

interface MediaHeroProps {
    image: string | null;
    /** The cover box art; defaults to the backdrop image. */
    coverImage?: string | null;
    alt: string;
    colors: ColorPalette | null;
    tint: "music" | "spoken";
    fallback: [string, string];
    shape: "square" | "circle";
    fallbackIcon: ReactNode;
    eyebrowDotClass: string;
    eyebrowLabel: string;
    title: string;
    /** Beside the title: the metadata edit controls. */
    titleExtra?: ReactNode;
    /** Spacing between title and edit controls; artist keeps the wider gap-3. */
    titleGap?: "gap-2" | "gap-3";
    /** The dot-separated meta line under the title. */
    meta: ReactNode;
    /** A block under the meta line: the genre chip. */
    metaExtra?: ReactNode;
    children?: ReactNode;
}

/** The music hero: backdrop, cover, eyebrow, title, meta. Album and artist share this skeleton. */
export function MediaHero({
    image,
    coverImage,
    alt,
    colors,
    tint,
    fallback,
    shape,
    fallbackIcon,
    eyebrowDotClass,
    eyebrowLabel,
    title,
    titleExtra,
    titleGap = "gap-2",
    meta,
    metaExtra,
    children,
}: MediaHeroProps) {
    return (
        <div className="relative animate-rise">
            <HeroBackdrop image={image} alt={alt} colors={colors} tint={tint} fallback={fallback} />

            <div className="relative page-shell pt-16 pb-6">
                <div className="flex items-end gap-6">
                    <HeroCover
                        image={coverImage ?? image}
                        alt={alt}
                        shape={shape}
                        fallbackIcon={fallbackIcon}
                    />

                    <div className="flex-1 min-w-0 pb-1">
                        <div className="flex items-center gap-2 mb-1">
                            <div className={cn("w-1.5 h-1.5 rounded-full", eyebrowDotClass)} />
                            <span className="t-eyebrow">{eyebrowLabel}</span>
                        </div>
                        <div className={cn("flex items-center group mb-2", titleGap)}>
                            <h1 className="text-2xl md:text-4xl lg:text-5xl font-bold tracking-tight text-white leading-tight line-clamp-2">
                                {title}
                            </h1>
                            {titleExtra}
                        </div>
                        <div className="flex flex-wrap items-center gap-1 text-sm text-[var(--text-secondary)] mb-1">
                            {meta}
                        </div>
                        {metaExtra}
                    </div>
                </div>
            </div>

            {children && <div className="relative page-shell pb-4">{children}</div>}
        </div>
    );
}
