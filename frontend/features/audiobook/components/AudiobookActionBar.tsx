"use client";

import { useState } from "react";
import { Play, Pause, RotateCcw, CheckCircle } from "lucide-react";
import type { Audiobook } from "../types";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";

interface AudiobookActionBarProps {
    audiobook: Audiobook;
    isThisBookPlaying: boolean;
    isPlaying: boolean;
    currentTime: number;
    onPlayPause: () => void;
    onResetProgress: () => void;
    onMarkAsCompleted: () => void;
    formatTime: (seconds: number) => string;
}

export function AudiobookActionBar({
    audiobook,
    isThisBookPlaying,
    isPlaying,
    currentTime,
    onPlayPause,
    onResetProgress,
    onMarkAsCompleted,
    formatTime,
}: AudiobookActionBarProps) {
    const hasProgress = audiobook.progress && audiobook.progress.progress > 0;
    const isFinished = audiobook.progress?.isFinished;
    const showingPause = isThisBookPlaying && isPlaying;
    const [confirmingReset, setConfirmingReset] = useState(false);

    return (
        <div className="flex items-center gap-4">
            <button
                onClick={onPlayPause}
                className="w-10 h-10 rounded-lg bg-brand hover:bg-brand-hover transition-all flex items-center justify-center hover:scale-105 active:scale-95"
                title={showingPause ? "pause" : (hasProgress && !isFinished ? "resume" : "play")}
            >
                {showingPause ? (
                    <Pause className="w-4 h-4 text-black" />
                ) : (
                    <Play className="w-4 h-4 text-black ml-0.5" fill="black" />
                )}
            </button>

            {hasProgress && !isFinished && (
                <div className="hidden sm:flex items-center gap-3">
                    <div className="text-xs tabular-nums text-[var(--text-secondary)]">
                        <span>
                            {formatTime(isThisBookPlaying ? currentTime : audiobook.progress!.currentTime)}
                        </span>
                        <span className="text-[var(--text-muted)] mx-1">/</span>
                        <span>{formatTime(audiobook.duration)}</span>
                    </div>
                    <div className="w-24 h-1 bg-white/10 rounded-full overflow-hidden">
                        <div
                            className="h-full bg-brand rounded-full transition-all"
                            style={{
                                width: `${isThisBookPlaying ? (currentTime / audiobook.duration) * 100 : audiobook.progress!.progress}%`,
                            }}
                        />
                    </div>
                </div>
            )}

            <div className="flex-1" />

            <div className="flex items-center gap-2">
                {hasProgress && !isFinished && (
                    <>
                        <button
                            onClick={() => setConfirmingReset(true)}
                            className="p-2 rounded-lg text-[var(--text-muted)] hover:text-white hover:bg-white/5 border border-transparent hover:border-white/10 transition-all"
                            title="reset progress"
                            aria-label="reset progress"
                        >
                            <RotateCcw className="w-4 h-4" />
                        </button>
                        <button
                            onClick={onMarkAsCompleted}
                            className="p-2 rounded-lg text-success/60 hover:text-success hover:bg-success/10 border border-transparent hover:border-success/20 transition-all"
                            title="mark as completed"
                        >
                            <CheckCircle className="w-4 h-4" />
                        </button>
                    </>
                )}

                {isFinished && (
                    <Button
                        variant="secondary"
                        size="sm"
                        onClick={onResetProgress}
                        title="listen again"
                    >
                        <RotateCcw className="w-3.5 h-3.5" />
                        <span>Listen Again</span>
                    </Button>
                )}
            </div>
            <ConfirmDialog
                isOpen={confirmingReset}
                onClose={() => setConfirmingReset(false)}
                onConfirm={() => { setConfirmingReset(false); onResetProgress(); }}
                title="Reset progress?"
                message={`Your place in ${audiobook.title}${currentTime > 0 ? ` (${formatTime(Math.floor(currentTime))})` : ""} is forgotten and the book starts from the beginning.`}
                confirmText="Reset"
                variant="warning"
            />
        </div>
    );
}
