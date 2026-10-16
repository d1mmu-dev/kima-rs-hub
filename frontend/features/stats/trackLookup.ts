"use client";

import { useEffect, useRef, useState } from "react";

/**
 * Builds a hook that fetches one value per track when the track changes and
 * keeps it for the session, so a queue you move around in asks once per track.
 * A value `keep` rejects is asked for again next time.
 */
export function createTrackLookup<T>(fetchValue: (trackId: string) => Promise<T>, keep: (value: T) => boolean = () => true) {
    const known = new Map<string, T>();

    return function useTrackLookup(trackId: string | null | undefined): T | null {
        // The result and the track it was for, kept together so a result can
        // never be shown under the wrong track.
        const [fetched, setFetched] = useState<{ id: string; value: T } | null>(null);
        // Only the newest request may write, so a fast skip through a queue
        // cannot land an earlier track's value under a later track's.
        const ticket = useRef(0);

        useEffect(() => {
            if (!trackId || known.has(trackId)) return;

            const mine = ++ticket.current;
            let cancelled = false;
            (async () => {
                try {
                    const value = await fetchValue(trackId);
                    if (cancelled || ticket.current !== mine) return;
                    if (keep(value)) known.set(trackId, value);
                    setFetched({ id: trackId, value });
                } catch {
                    // A value that cannot be read is a line that does not appear.
                }
            })();
            return () => { cancelled = true; };
        }, [trackId]);

        // Derived during render: a missing track and an already-known one need
        // no state write, which keeps this off the cascading-render path.
        if (!trackId) return null;
        const cached = known.get(trackId);
        if (cached !== undefined) return cached;
        return fetched?.id === trackId ? fetched.value : null;
    };
}
