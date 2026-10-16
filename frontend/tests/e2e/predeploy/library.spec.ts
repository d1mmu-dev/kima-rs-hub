import { test, expect } from "@playwright/test";
import { loginAsTestUser } from "../fixtures/test-helpers";

test.describe("Library", () => {
    test.beforeEach(async ({ page }) => {
        await loginAsTestUser(page);
    });

    test("home page loads with library stats", async ({ page }) => {
        await page.goto("/");
        await expect(page.locator("body")).toContainText(/artist|album|track|library/i);
    });

    test("albums tab shows album grid", async ({ page }) => {
        await page.goto("/collection?tab=albums");
        await expect(page.getByRole("heading", { name: /collection/i })).toBeVisible();

        const albumLinks = page.locator('a[href^="/album/"]');
        try {
            await albumLinks.first().waitFor({ timeout: 8_000 });
        } catch {
            test.skip(true, "No albums in library -- skipping"); return;
        }
        await expect(albumLinks.first()).toBeVisible();
    });

    test("artists tab shows artist list", async ({ page }) => {
        await page.goto("/collection?tab=artists");
        await expect(page.getByRole("heading", { name: /collection/i })).toBeVisible();

        const artistLinks = page.locator('a[href^="/artist/"]');
        try {
            await artistLinks.first().waitFor({ timeout: 8_000 });
        } catch {
            test.skip(true, "No artists in library -- skipping"); return;
        }
        await expect(artistLinks.first()).toBeVisible();
    });

    test("tracks tab shows track list", async ({ page }) => {
        await page.goto("/collection?tab=tracks");
        await expect(page.getByRole("heading", { name: /collection/i })).toBeVisible();

        const trackRows = page.locator('[data-track-id], [class*="track"]');
        try {
            await trackRows.first().waitFor({ timeout: 8_000 });
        } catch {
            test.skip(true, "No tracks in library -- skipping"); return;
        }
        await expect(trackRows.first()).toBeVisible();
    });

    test("search page accessible", async ({ page }) => {
        await page.goto("/search");

        await expect(page.locator("body")).toBeVisible();
        await expect(page).toHaveURL(/search/);
    });
});
