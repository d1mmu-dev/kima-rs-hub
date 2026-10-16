"use client";

import { ReactNode } from "react";
import { ErrorBoundary } from "@/components/providers/ErrorBoundary";
import { Button } from "@/components/ui/Button";

/** Catches errors in the audio providers so the rest of the app keeps running. */
export function AudioErrorBoundary({ children }: { children: ReactNode }) {
    return (
        <ErrorBoundary
            label="AudioErrorBoundary"
            renderFallback={() => (
                <div className="flex flex-col items-center justify-center gap-4 p-8 text-center">
                    <p className="text-[var(--text-secondary)]">Playback system error</p>
                    <Button variant="primary" onClick={() => window.location.reload()}>
                        Reload
                    </Button>
                </div>
            )}
        >
            {children}
        </ErrorBoundary>
    );
}
