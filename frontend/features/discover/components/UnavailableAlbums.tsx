import { useState } from "react";
import { Play, Pause, Music, ChevronDown, ChevronUp, RefreshCw } from "lucide-react";
import { Card } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { cn } from "@/lib/cn";
import { UnavailableAlbum } from "../types";
import { tierColors, tierLabels } from "../constants";

interface UnavailableAlbumsProps {
    unavailable: UnavailableAlbum[];
    currentPreview: string | null;
    onTogglePreview: (albumId: string, artistName: string, trackTitle: string) => void;
    onRetryAll?: () => void;
    isRetrying?: boolean;
}

export function UnavailableAlbums({
    unavailable,
    currentPreview,
    onTogglePreview,
    onRetryAll,
    isRetrying,
}: UnavailableAlbumsProps) {
    const [isExpanded, setIsExpanded] = useState(false);

    if (!unavailable || unavailable.length === 0) {
        return null;
    }

    return (
        <Card>
            <div className="p-4 flex items-center justify-between">
                <button
                    onClick={() => setIsExpanded(!isExpanded)}
                    className="flex items-center gap-2 hover:bg-[var(--bg-hover)] transition-colors rounded-lg flex-1"
                >
                    <Music className="w-5 h-5 text-warning" />
                    <span className="text-sm font-medium text-[var(--text-secondary)]">
                        {unavailable.length} album{unavailable.length !== 1 ? "s" : ""} unavailable
                    </span>
                    {isExpanded ? (
                        <ChevronUp className="w-4 h-4 text-[var(--text-muted)]" />
                    ) : (
                        <ChevronDown className="w-4 h-4 text-[var(--text-muted)]" />
                    )}
                </button>
                {onRetryAll && (
                    <Button
                        variant="ghost"
                        size="sm"
                        onClick={(e) => { e.stopPropagation(); onRetryAll(); }}
                        disabled={isRetrying}
                        className="text-warning hover:text-warning hover:bg-warning/10"
                    >
                        <RefreshCw className={cn("w-3 h-3", isRetrying && "animate-spin")} />
                        Retry All
                    </Button>
                )}
            </div>
            {isExpanded && (
                <>
                    <div className="px-6 pb-4">
                        <p className="text-sm text-[var(--text-secondary)]">
                            These albums were recommended but couldn&apos;t be found by your indexers.
                            Listen to 30-second previews below!
                        </p>
                    </div>
                    <div className="divide-y divide-[#1c1c1c]">
                {unavailable.map((album) => {
                    const isPreviewPlaying = currentPreview === album.id;
                    const attemptLabel =
                        album.attemptNumber === 0
                            ? "Original Recommendation"
                            : `Replacement #${album.attemptNumber}`;

                    return (
                        <div
                            key={album.id}
                            className={cn(
                                "flex items-center gap-4 px-4 py-3 hover:bg-[var(--bg-hover)] transition-colors group",
                                (album.attemptNumber ?? 0) > 0 &&
                                    "pl-12 bg-[var(--bg-hover)]/30"
                            )}
                        >
                            <div className="w-8 flex items-center justify-center">
                                {album.previewUrl ? (
                                    <button
                                        onClick={() =>
                                            onTogglePreview(
                                                album.id,
                                                album.artist,
                                                album.title
                                            )
                                        }
                                        className="w-8 h-8 flex items-center justify-center"
                                    >
                                        {isPreviewPlaying ? (
                                            <Pause className="w-4 h-4 text-warning fill-current" />
                                        ) : (
                                            <Play className="w-4 h-4 text-warning fill-current ml-0.5" />
                                        )}
                                    </button>
                                ) : (
                                    <Music className="w-4 h-4 text-[var(--text-muted)]" />
                                )}
                            </div>

                            <div className="w-12 h-12 bg-[#181818] rounded flex items-center justify-center shrink-0">
                                <Music className="w-6 h-6 text-[var(--text-muted)]" />
                            </div>

                            <div className="flex-1 min-w-0">
                                <h3 className="text-sm font-medium text-white truncate">
                                    {album.album}
                                </h3>
                                <div className="flex items-center gap-2 text-xs text-[var(--text-secondary)] truncate">
                                    <span>{album.artist}</span>
                                    {album.previewUrl && (
                                        <>
                                            <span>•</span>
                                            <span className="text-warning">
                                                30s Preview
                                            </span>
                                        </>
                                    )}
                                </div>
                            </div>

                            <div className="hidden md:flex items-center gap-2">
                                <span
                                    className={cn(
                                        "px-2 py-1 rounded-full text-xs font-medium bg-white/5",
                                        tierColors[album.tier]
                                    )}
                                >
                                    {tierLabels[album.tier]}
                                </span>
                            </div>

                            <div className="flex items-center gap-2">
                                <span
                                    className={cn(
                                        "px-2 py-1 rounded-full text-xs font-medium whitespace-nowrap",
                                        album.attemptNumber === 0
                                            ? "bg-warning/20 border border-warning/30 text-warning"
                                            : "bg-white/10 border border-white/10 text-[var(--text-secondary)]"
                                    )}
                                >
                                    {attemptLabel}
                                </span>
                            </div>
                        </div>
                    );
                })}
                    </div>
                </>
            )}
        </Card>
    );
}
