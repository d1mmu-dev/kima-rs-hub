"use client";

import Link from "next/link";
import { Music as MusicIcon } from "lucide-react";
import { CachedImage } from "@/components/ui/CachedImage";
import { cn } from "@/lib/cn";

interface PlayerCoverProps {
    href: string | null;
    coverUrl: string | null | undefined;
    title: string;
    isPlaying: boolean;
    className: string;
}

/** The cover tile in the docked players: the playing item's art, linking to the item when it has a page. */
export function PlayerCover({ href, coverUrl, title, isPlaying, className }: PlayerCoverProps) {
    const tile = (
        <div className={cn("relative w-full h-full bg-gradient-to-br from-[#2a2a2a] to-[#1a1a1a] rounded-lg overflow-hidden flex items-center justify-center", "transition-transform duration-300", isPlaying && "scale-[1.03]")}>
            {coverUrl ? (
                <CachedImage
                    key={coverUrl}
                    src={coverUrl}
                    alt={title}
                    fill
                    sizes="56px"
                    className="object-cover"
                    priority
                />
            ) : (
                <MusicIcon className="w-6 h-6 text-[var(--text-muted)]" />
            )}
        </div>
    );
    const classes = cn("relative flex-shrink-0", className);
    return href ? (
        <Link href={href} className={classes}>
            {tile}
        </Link>
    ) : (
        <div className={classes}>{tile}</div>
    );
}
