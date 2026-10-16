import { describe, expect, it } from "vitest";
import { BRAND_GRADIENT_CSS, BRAND_STOPS, brandColourAt } from "@/lib/brandGradient";

describe("brand gradient", () => {
    it("runs from the logo's azure end at 0 to its amber end at 1", () => {
        expect(brandColourAt(0)).toEqual([0x01, 0x91, 0xc9]);
        expect(brandColourAt(1)).toEqual([0xfc, 0xa5, 0x06]);
    });

    it("hits each sampled colour at its own place and blends between them", () => {
        for (const [position, hex] of BRAND_STOPS) {
            const rgb = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
            expect(brandColourAt(1 - position)).toEqual(rgb);
        }
        const [r, , b] = brandColourAt(0.5);
        expect(r).toBeGreaterThan(100);
        expect(b).toBeGreaterThan(100);
    });

    it("keeps out-of-range places at the ends", () => {
        expect(brandColourAt(-1)).toEqual(brandColourAt(0));
        expect(brandColourAt(2)).toEqual(brandColourAt(1));
    });

    it("draws the mark with the same stops, amber at the top", () => {
        expect(BRAND_GRADIENT_CSS.startsWith("linear-gradient(180deg, #fca506 2%")).toBe(true);
        expect(BRAND_GRADIENT_CSS.endsWith("#0191c9 98%)")).toBe(true);
    });
});
