"use client";

import { useLayoutEffect, useRef } from "react";

/** Below this the text stops being readable, so the panel clips instead of shrinking further. */
const MIN_SCALE = 0.5;

/**
 * Shows its content at the largest scale, up to 1, that fits without scrolling,
 * laid out at width / scale so it still spans the panel. `fill` content takes
 * the full height and pages itself. `--fit-h` is the height on offer to charts.
 */
export function FitPanel({ fill = false, children }: { fill?: boolean; children: React.ReactNode }) {
    const outer = useRef<HTMLDivElement>(null);
    const inner = useRef<HTMLDivElement>(null);

    useLayoutEffect(() => {
        const box = outer.current;
        const content = inner.current;
        if (!box || !content) return;

        const fits = (scale: number, width: number, height: number) => {
            content.style.width = `${width / scale}px`;
            content.style.setProperty("--fit-h", `${height / scale}px`);
            if (!fill) return content.offsetHeight * scale <= height;
            content.style.height = `${height / scale}px`;
            return content.scrollHeight <= Math.ceil(height / scale);
        };

        let frame = 0;
        const fit = () => {
            frame = 0;
            const width = box.clientWidth;
            const height = box.clientHeight;
            if (!width || !height) return;
            let scale = 1;
            if (!fits(1, width, height)) {
                let lo = MIN_SCALE;
                let hi = 1;
                for (let step = 0; step < 8; step++) {
                    const mid = (lo + hi) / 2;
                    if (fits(mid, width, height)) lo = mid;
                    else hi = mid;
                }
                scale = lo;
                fits(scale, width, height);
            }
            content.style.transform = scale < 1 ? `scale(${scale})` : "";
        };
        const schedule = () => {
            if (!frame) frame = requestAnimationFrame(fit);
        };

        const resized = new ResizeObserver(schedule);
        resized.observe(box);
        resized.observe(content);
        const changed = new MutationObserver(schedule);
        changed.observe(content, { childList: true, subtree: true, characterData: true });
        fit();
        return () => {
            resized.disconnect();
            changed.disconnect();
            cancelAnimationFrame(frame);
        };
    }, [fill]);

    return (
        <div ref={outer} className="relative h-full w-full overflow-hidden">
            <div ref={inner} className="absolute left-0 top-0 origin-top-left">
                {children}
            </div>
        </div>
    );
}
