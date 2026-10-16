"use client";

import { ReactNode, Suspense } from "react";
import { usePlayerMode } from "@/hooks/usePlayerMode";

// usePlayerMode reads search params, which a static export only allows below a
// Suspense boundary. A separate null-rendering component keeps `children`
// outside that boundary.
function PlayerModeSync() {
    usePlayerMode();
    return null;
}

export function PlayerModeWrapper({ children }: { children: ReactNode }) {
    return (
        <>
            <Suspense fallback={null}>
                <PlayerModeSync />
            </Suspense>
            {children}
        </>
    );
}
