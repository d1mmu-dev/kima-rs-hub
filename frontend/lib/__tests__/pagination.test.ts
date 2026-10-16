import { describe, it, expect } from "vitest";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { latchTotal, parsePageParam } from "../pagination";

describe("parsePageParam", () => {
    it("falls back to 1 for missing, empty or unparseable values", () => {
        expect(parsePageParam(null)).toBe(1);
        expect(parsePageParam("")).toBe(1);
        expect(parsePageParam("abc")).toBe(1);
        expect(parsePageParam("0")).toBe(1);
        expect(parsePageParam("-3")).toBe(1);
    });

    it("floors fractional pages and accepts the rest", () => {
        expect(parsePageParam("2.9")).toBe(2);
        expect(parsePageParam("7")).toBe(7);
    });
});

describe("latchTotal", () => {
    it("keeps the last known total when the fresh response lost it", () => {
        expect(latchTotal(120, null)).toBe(120);
        expect(latchTotal(120, undefined)).toBe(120);
    });

    it("takes a genuine fresh zero", () => {
        expect(latchTotal(120, 0)).toBe(0);
    });

    it("seeds from zero with no history", () => {
        expect(latchTotal(0, 120)).toBe(120);
        expect(latchTotal(0, null)).toBe(0);
    });
});

describe("the freeze contract", () => {
    it("placeholderData does not survive a failed first load, so the page must latch the total", async () => {
        const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
        type Total = { total: number };
        let fail = false;
        const queryFn = async (): Promise<Total> => {
            if (fail) throw new Error("boom");
            return { total: 120 };
        };
        const placeholderData = (prev: Total | undefined) => prev;
        const observer = new QueryObserver<Total, Error, Total, Total, (string | number)[]>(qc, {
            queryKey: ["freeze-contract"],
            queryFn,
            placeholderData,
        });
        const first = await observer.refetch();
        fail = true;
        // setOptions REPLACES options: queryFn/placeholderData must be carried
        // or the error is "Missing queryFn" and the test proves nothing.
        observer.setOptions({ queryKey: ["freeze-contract", 2], queryFn, placeholderData });
        const second = await observer.refetch();

        expect(second.status).toBe("error");
        expect(second.error?.message).toBe("boom"); // genuine fetch failure, not Missing queryFn
        expect(second.data).toBeUndefined();
        // The page's contract: from (latched=120, fresh=undefined) the pager
        // still says 3 pages, where `data?.total ?? 0` said 0 and unmounted it.
        expect(Math.ceil(latchTotal(first.data?.total ?? 0, second.data?.total) / 40)).toBe(3);
        expect(Math.ceil((second.data?.total ?? 0) / 40)).toBe(0);
    });
});