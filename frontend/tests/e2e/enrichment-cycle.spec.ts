import { test, expect } from "@playwright/test";
import { loginAsTestUser, getAuthToken } from "./fixtures/test-helpers";

// Wipes enrichment data, re-enriches, and checks the library still works.
// Skips below 10 tracks. scripts/run-enrichment-memory-test.sh runs it with
// memory monitoring.

const POLL_INTERVAL_MS = 10_000;
const ENRICHMENT_TIMEOUT_MS = 45 * 60 * 1000;

type EnrichmentStatus = {
    status: string;
    currentPhase: string | null;
    completionNotificationSent: boolean;
    tracks: { total: number; completed: number; failed: number };
    artists: { total: number; completed: number; failed: number };
    audio: { total: number; completed: number; failed: number; processing: number };
};

/** Poll /api/enrichment/status until idle+complete or timeout.
 *  Returns the final status object. */
async function waitForEnrichment(
    page: Parameters<typeof loginAsTestUser>[0],
    token: string,
    timeoutMs: number,
): Promise<EnrichmentStatus> {
    const deadline = Date.now() + timeoutMs;
    let last: EnrichmentStatus | null = null;

    while (Date.now() < deadline) {
        await page.waitForTimeout(POLL_INTERVAL_MS);

        try {
            const res = await page.request.get("/api/enrichment/status", {
                headers: { Authorization: `Bearer ${token}` },
            });
            if (!res.ok()) continue;

            const s = (await res.json()) as EnrichmentStatus;
            last = s;

            const elapsed = Math.round((Date.now() - (deadline - timeoutMs)) / 1000);
            const pct =
                s.tracks.total > 0
                    ? Math.round((s.tracks.completed / s.tracks.total) * 100)
                    : 0;
            const phase = s.currentPhase ? ` phase=${s.currentPhase}` : "";
            console.log(
                `[${elapsed}s] status=${s.status}${phase} | ` +
                `tracks=${s.tracks.completed}/${s.tracks.total} (${pct}%) | ` +
                `artists=${s.artists.completed}/${s.artists.total} | ` +
                `audio=${s.audio.completed}/${s.audio.total}`,
            );

            if (s.status === "idle" && s.completionNotificationSent) {
                return s;
            }
        } catch {
            // transient error -- keep polling
        }
    }

    throw new Error(
        `Enrichment did not complete within ${timeoutMs / 60_000} minutes. ` +
        `Last status: ${JSON.stringify(last)}`,
    );
}

test.describe("Enrichment Cycle", () => {
    test.beforeEach(async ({ page }) => {
        await loginAsTestUser(page);
    });

    test("wipe and re-enrich: system is functional after full enrichment cycle", async ({ page }) => {
        test.setTimeout(55 * 60 * 1000);

        const token = await getAuthToken(page);

        // Stop any currently running enrichment before wiping
        await page.request.post("/api/enrichment/stop", {
            headers: { Authorization: `Bearer ${token}` },
        });
        await page.waitForTimeout(2_000);

        const resetRes = await page.request.post("/api/enrichment/reset-all", {
            headers: { Authorization: `Bearer ${token}` },
        });
        expect(resetRes.ok()).toBe(true);
        const resetData = (await resetRes.json()) as { tracksReset: number; artistsReset: number };

        if (resetData.tracksReset < 10) {
            test.skip(
                true,
                `Library too small (${resetData.tracksReset} tracks) -- skipping enrichment cycle (empty container)`,
            );
            return;
        }

        console.log(
            `Reset complete: ${resetData.tracksReset} tracks, ${resetData.artistsReset} artists cleared`,
        );

        // Confirm completionNotificationSent is now falsy (null or false after reset)
        const statusAfterReset = await page.request.get("/api/enrichment/status", {
            headers: { Authorization: `Bearer ${token}` },
        });
        const resetStatus = (await statusAfterReset.json()) as EnrichmentStatus;
        expect(resetStatus.completionNotificationSent).toBeFalsy();

        const startRes = await page.request.post("/api/enrichment/full", {
            headers: { Authorization: `Bearer ${token}` },
        });
        expect(startRes.ok()).toBe(true);
        console.log("Full enrichment started");

        const finalStatus = await waitForEnrichment(page, token, ENRICHMENT_TIMEOUT_MS);

        console.log(
            `Enrichment complete: tracks ${finalStatus.tracks.completed}/${finalStatus.tracks.total} | ` +
            `audio ${finalStatus.audio.completed}/${finalStatus.audio.total} | ` +
            `failures ${finalStatus.tracks.failed + finalStatus.artists.failed}`,
        );

        // ---- Functional assertions after enrichment -------------------------

        // 1. At least 80% of tracks should have completed enrichment
        const trackSuccessRate =
            finalStatus.tracks.total > 0
                ? finalStatus.tracks.completed / finalStatus.tracks.total
                : 0;
        expect(trackSuccessRate).toBeGreaterThanOrEqual(0.8);

        // 2. Vibe map should return embedded tracks
        const vibeRes = await page.request.get("/api/vibe/map", {
            headers: { Authorization: `Bearer ${token}` },
        });
        expect(vibeRes.ok()).toBe(true);
        const vibeData = (await vibeRes.json()) as { tracks: unknown[]; trackCount: number };
        expect(Array.isArray(vibeData.tracks)).toBe(true);
        expect(vibeData.tracks.length).toBeGreaterThan(0);

        // 3. Vibe search returns the nearest tracks with no cut-off; probe several
        //    descriptors and require at least one to return results.
        const searchCandidates = ["rock", "pop", "electronic", "loud", "bright", "guitar", "fast", "sad", "piano"];
        let searchHit = false;
        for (const q of searchCandidates) {
            const r = await page.request.post("/api/vibe/search", {
                data: { query: q, limit: 3 },
                headers: { Authorization: `Bearer ${token}` },
            });
            if (!r.ok()) continue;
            const d = (await r.json()) as { tracks: unknown[] };
            if (d.tracks.length > 0) { searchHit = true; break; }
        }
        expect(searchHit).toBe(true);

        // 4. Failure rate should be below 20%
        const failRes = await page.request.get("/api/enrichment/failures/counts", {
            headers: { Authorization: `Bearer ${token}` },
        });
        if (failRes.ok()) {
            const fails = (await failRes.json()) as Record<string, number>;
            const totalFails = Object.values(fails).reduce((a, b) => a + b, 0);
            const failRate = resetData.tracksReset > 0 ? totalFails / resetData.tracksReset : 0;
            expect(failRate).toBeLessThan(0.2);
        }
    });
});
