import { test, expect } from "@playwright/test";
import {
    loginAsTestUser,
    startPlayingFirstAlbum,
    getAudioSrc,
    getAudioCurrentTime,
    setAudioCurrentTime,
    waitForSrcChange,
    seekToPercent,
} from "../fixtures/test-helpers";
import { PLAYER } from "../fixtures/shared";

test.describe("Playback", () => {
    test.beforeEach(async ({ page }) => {
        await loginAsTestUser(page);
    });

    test("audio starts when Play all is clicked", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const src = await getAudioSrc(page);
        expect(src).toBeTruthy();
        expect(src).toMatch(/\/api\/|stream|audio/i);

        await expect(page.getByTitle("pause", { exact: true })).toBeVisible();
    });

    test("stream request is made when playback starts", async ({ page }) => {
        const audioRequests: string[] = [];
        page.on("request", (req) => {
            const url = req.url();
            if (req.resourceType() === "media" || url.includes("/stream") || url.includes("/audio")) {
                audioRequests.push(url);
            }
        });

        await startPlayingFirstAlbum(page);

        expect(audioRequests.length).toBeGreaterThan(0);
    });

    test("play/pause toggle works", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        // title="pause"/"play" are set only on the FullPlayer button (unambiguous)
        const pauseBtn = page.getByTitle("pause", { exact: true });
        await expect(pauseBtn).toBeVisible();
        await pauseBtn.click();

        const playBtn = page.getByTitle("play", { exact: true });
        await expect(playBtn).toBeVisible();

        await playBtn.click();
        await expect(page.getByTitle("pause", { exact: true })).toBeVisible();
    });

    test("next track changes audio src", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const srcBefore = await getAudioSrc(page);
        await page.getByLabel("Next track").click();
        const srcAfter = await waitForSrcChange(page, srcBefore);

        expect(srcAfter).not.toBe(srcBefore);
        expect(srcAfter).toBeTruthy();
    });

    test("previous track when currentTime > 3s restarts the same track", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await setAudioCurrentTime(page, 10);

        const srcBefore = await getAudioSrc(page);
        await page.getByLabel("Previous track").click();

        await page.waitForTimeout(500);

        const srcAfter = await getAudioSrc(page);
        expect(srcAfter).toBe(srcBefore);

        const currentTime = await getAudioCurrentTime(page);
        expect(currentTime).toBeLessThan(3);
    });

    test("previous track when currentTime <= 3s goes to prior track", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const src1 = await getAudioSrc(page);
        await page.getByLabel("Next track").click();
        await waitForSrcChange(page, src1);

        // Now we are at track 2, currentTime should be near 0
        // (no setAudioCurrentTime -- we want it under the 3s threshold)
        const src2 = await getAudioSrc(page);

        await page.getByLabel("Previous track").click();
        const src3 = await waitForSrcChange(page, src2, 8_000);

        expect(src3).not.toBe(src2);
    });

    test("seek bar changes playback position", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await setAudioCurrentTime(page, 30);
        await page.waitForTimeout(200);

        await seekToPercent(page, 10);

        const timeBefore = 30;
        const timeAfter = await getAudioCurrentTime(page);

        // 10% of most tracks is less than 30s
        expect(timeAfter).toBeLessThan(timeBefore);
    });

    test("playback continues to next track automatically", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const srcBefore = await getAudioSrc(page);

        // Jump to the last second and let the track end on its own: no Next press.
        await page.evaluate(async (sel) => {
            const audio = document.querySelector(sel) as HTMLAudioElement;
            if (!Number.isFinite(audio.duration)) {
                await new Promise((resolve) => audio.addEventListener("loadedmetadata", resolve, { once: true }));
            }
            audio.currentTime = Math.max(0, audio.duration - 1);
        }, PLAYER);
        const srcAfter = await waitForSrcChange(page, srcBefore, 15_000);

        expect(srcAfter).not.toBe(srcBefore);
        await expect(page.getByTitle("pause", { exact: true })).toBeVisible();
    });

    test("playback state persists across client-side navigation", async ({ page }) => {
        await startPlayingFirstAlbum(page);
        const srcBefore = await getAudioSrc(page);

        // Use client-side navigation (click a sidebar/nav link) to preserve React state.
        // Playwright's page.goto() does a full page reload which wipes React context.
        const homeLink = page.locator('a[href="/"]').first();
        if (await homeLink.isVisible()) {
            await homeLink.click();
        } else {
            // Fallback: use history.pushState to navigate without reload
            await page.evaluate(() => window.history.pushState({}, "", "/"));
        }
        await page.waitForTimeout(500);

        // The same track is still playing, not just still loaded: React state survives client-side route changes.
        const srcAfter = await getAudioSrc(page);
        expect(srcAfter).toBe(srcBefore);
        const timeBefore = await getAudioCurrentTime(page);
        await page.waitForTimeout(1_500);
        expect(await getAudioCurrentTime(page), "playback should still be moving after navigating").toBeGreaterThan(timeBefore);
    });
});
