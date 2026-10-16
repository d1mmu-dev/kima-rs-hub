import { test, expect, Page } from "@playwright/test";
import {
    loginAsTestUser,
    startPlayingFirstAlbum,
    getAudioSrc,
    waitForSrcChange,
} from "./fixtures/test-helpers";

/** Opens the queue through the sidebar link: page.goto reloads the page and wipes the in-memory queue. */
async function openQueueByClick(page: Page): Promise<void> {
    await page.getByRole("link", { name: "Queue" }).click();
    await page.waitForURL(/\/queue/);
}

test.describe("Queue", () => {
    test.beforeEach(async ({ page }) => {
        await loginAsTestUser(page);
    });

    test("Add to queue button appends track to up-next list", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const firstTrackRow = page.locator("[data-track-row]").first();
        await firstTrackRow.waitFor({ timeout: 10_000 });
        await firstTrackRow.hover();
        await firstTrackRow.getByLabel("Add to queue").click();

        await openQueueByClick(page);

        // "Next Up" section must appear since the album has multiple tracks ahead
        await expect(page.getByText(/Next Up/)).toBeVisible({ timeout: 5_000 });
    });

    test("queue page loads with Now Playing section when track is active", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await openQueueByClick(page);

        await expect(page.getByRole("heading", { name: "Now Playing" })).toBeVisible({ timeout: 5_000 });
    });

    test("queue page shows track count", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await page.goto("/queue");
        await page.waitForLoadState("domcontentloaded");

        // The queue header shows "{n} track(s) in queue"
        await expect(page.locator("text=/\\d+ track/")).toBeVisible({ timeout: 5_000 });
    });

    test("Play now jumps to selected queue item", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        const srcBefore = await getAudioSrc(page);

        await openQueueByClick(page);
        // Each queued row has "Play <title>"; the player's own button is plain "Play"/"Pause".
        const playNowButtons = page.getByRole("button", { name: /^Play .+/ });
        await expect(playNowButtons.first(), "a started album leaves tracks up next").toBeAttached({ timeout: 5_000 });
        await playNowButtons.first().click();

        const srcAfter = await waitForSrcChange(page, srcBefore, 8_000);
        expect(srcAfter).not.toBe(srcBefore);
    });

    test("Remove removes item from queue", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await openQueueByClick(page);

        const removeButtons = page.getByRole("button", { name: /^Remove .+ from queue$/ });
        await expect(removeButtons.first(), "a started album leaves tracks up next").toBeAttached({ timeout: 5_000 });
        const initialCount = await removeButtons.count();

        await removeButtons.first().click();

        await expect(removeButtons).toHaveCount(initialCount - 1, { timeout: 3_000 });
    });

    test("Move up reorders queue item", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await openQueueByClick(page);

        const moveUpButtons = page.getByRole("button", { name: /^Move .+ up$/, disabled: false });
        await expect(moveUpButtons.first(), "the second track up next can move up").toBeAttached({ timeout: 5_000 });

        // Grab the track titles from "Next Up" before the reorder.
        // The first item's Move up button is always disabled, so the first
        // ENABLED button belongs to index 1 -- that's the track we'll move.
        const nextUpSection = page.locator("section").filter({
            has: page.locator("h2", { hasText: "Next Up" }),
        });
        const titlesBefore = await nextUpSection.locator("h3").allTextContents();
        expect(titlesBefore.length, "two tracks up next").toBeGreaterThanOrEqual(2);
        const targetTitle = titlesBefore[1]; // the one that will move to index 0

        await moveUpButtons.first().click();

        await expect.poll(async () => (await nextUpSection.locator("h3").allTextContents())[0]).toBe(targetTitle);
    });

    test("Clear queue empties the up-next list", async ({ page }) => {
        await startPlayingFirstAlbum(page);

        await openQueueByClick(page);

        // Clear queue only appears when queue.length > 0, and asks before clearing
        const clearBtn = page.getByRole("button", { name: "Clear queue" });
        await expect(clearBtn).toBeVisible({ timeout: 5_000 });
        await clearBtn.click();
        await page.getByRole("dialog", { name: "Clear the queue?" }).getByRole("button", { name: "Clear queue" }).click();

        await expect(page.getByText("No tracks in queue")).toBeVisible({ timeout: 5_000 });
    });
});
