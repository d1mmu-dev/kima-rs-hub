import { test, expect, type APIRequestContext, type Page } from "@playwright/test";

// Runs against a kima started with KIMA_IDENTIFY=off (outbound lookups locked off).
const USER = { username: "smokeadmin", password: "smoke-pass-1" };

async function signIn(page: Page, request: APIRequestContext) {
    let res = await request.post("/api/auth/login", { data: USER });
    if (!res.ok()) {
        // Run on its own against a fresh install: create the admin first.
        const reg = await request.post("/api/onboarding/register", { data: USER });
        expect(reg.ok()).toBeTruthy();
        res = await request.post("/api/auth/login", { data: USER });
    }
    expect(res.ok()).toBeTruthy();
    const body = await res.json();
    await request.post("/api/onboarding/complete", {
        headers: { Authorization: `Bearer ${body.token}` },
    });
    await page.addInitScript((v) => {
        localStorage.setItem("auth_token", v.token);
        localStorage.setItem("refresh_token", v.refreshToken);
    }, body);
}

test("AcoustID key: saved, shown as Saved, removed", async ({ page, request }) => {
    await signIn(page, request);
    // v1.12.3 splits settings into pages: artwork keys are on the server page.
    await page.goto("/settings/server/");

    const key = page.getByPlaceholder("Optional: Your AcoustID API key");
    await key.fill("test-acoustid-key");
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();

    await page.reload();
    const saved = page.getByPlaceholder("Saved — type to replace");
    await expect(saved).toBeVisible();
    await expect(saved).toHaveValue("");

    await page.getByRole("button", { name: "Remove" }).click();
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();

    await page.reload();
    await expect(page.getByPlaceholder("Saved — type to replace")).toHaveCount(0);
});

test("Library Enrichment: the toggle is locked by KIMA_IDENTIFY=off", async ({ page, request }) => {
    await signIn(page, request);
    // The enrichment controls are on the library settings page in v1.12.3.
    await page.goto("/settings/library/");
    await expect(page.locator("#auto-enrich")).toBeDisabled();
    await expect(page.getByText("Turned off on the server (KIMA_IDENTIFY=off)")).toBeVisible();
});
