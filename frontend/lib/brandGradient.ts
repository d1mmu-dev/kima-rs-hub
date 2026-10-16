/**
 * The Kima logo's gradient, sampled down the centre bar of the new Kima image: amber at the top through rose and
 * violet to azure. The logo mark draws it top to bottom; the vibe map colours each area by its place on it, 0 the
 * azure end and 1 the amber end (backend services/matching/mapLayout.ts colourField).
 */
export const BRAND_STOPS: ReadonlyArray<readonly [position: number, hex: string]> = [
    [0.02, "#fca506"], [0.09, "#fdac15"], [0.17, "#fdb530"], [0.24, "#fcb253"], [0.31, "#f69472"],
    [0.39, "#e86690"], [0.46, "#cd45aa"], [0.54, "#a633c2"], [0.61, "#7731d2"], [0.69, "#483ad9"],
    [0.76, "#234ed9"], [0.83, "#0e6bd3"], [0.91, "#0583cd"], [0.98, "#0191c9"],
];

/** The gradient as CSS, top (amber) to bottom (azure). */
export const BRAND_GRADIENT_CSS = `linear-gradient(180deg, ${BRAND_STOPS.map(([p, hex]) => `${hex} ${Math.round(p * 100)}%`).join(", ")})`;

function rgb(hex: string): [number, number, number] {
    return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number];
}

/** RGB (0–255) at `share` along the gradient: 0 the azure end, 1 the amber end. */
export function brandColourAt(share: number): [number, number, number] {
    const t = 1 - Math.min(1, Math.max(0, share));
    const first = BRAND_STOPS[0];
    const last = BRAND_STOPS[BRAND_STOPS.length - 1];
    if (t <= first[0]) return rgb(first[1]);
    if (t >= last[0]) return rgb(last[1]);
    for (let i = 1; i < BRAND_STOPS.length; i++) {
        const [p1, h1] = BRAND_STOPS[i];
        if (t <= p1) {
            const [p0, h0] = BRAND_STOPS[i - 1];
            const f = (t - p0) / (p1 - p0);
            const a = rgb(h0);
            const b = rgb(h1);
            return [0, 1, 2].map((c) => Math.round(a[c] + (b[c] - a[c]) * f)) as [number, number, number];
        }
    }
    return rgb(last[1]);
}
