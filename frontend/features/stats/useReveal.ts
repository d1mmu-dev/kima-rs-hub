"use client";

/**
 * Adds the reveal class once an element scrolls into view, then disconnects so
 * the entrance never replays. Reduced motion is handled globally.
 */

import { useEffect, useRef, useState } from "react";

export function useReveal<T extends HTMLElement = HTMLDivElement>(options?: {
    /** How much of it has to be showing. A whole section is never fully on screen. */
    threshold?: number;
    /** Start a little before the edge, so it is already moving as it appears. */
    rootMargin?: string;
}) {
    const ref = useRef<T | null>(null);
    const [shown, setShown] = useState(false);

    /** No IntersectionObserver: reveal at once. False on the server so hydration matches. */
    const noObserver =
        typeof window !== "undefined" && typeof IntersectionObserver === "undefined";

    useEffect(() => {
        const node = ref.current;
        if (!node || shown) return;

        // No observer (old browser, a test renderer): nothing to subscribe to.
        // `noObserver` above already reports it as shown, so there is no state
        // to write here — a missing entrance is a non-event, but hidden content
        // is a broken page.
        if (noObserver) return;

        const observer = new IntersectionObserver(
            (entries) => {
                if (entries.some((e) => e.isIntersecting)) {
                    setShown(true);
                    observer.disconnect();
                }
            },
            {
                threshold: options?.threshold ?? 0.12,
                rootMargin: options?.rootMargin ?? "0px 0px -8% 0px",
            },
        );
        observer.observe(node);
        return () => observer.disconnect();
    }, [shown, noObserver, options?.threshold, options?.rootMargin]);

    // Without an observer the effect never sets `shown`; this keeps sections visible.
    return { ref, shown: shown || noObserver };
}
