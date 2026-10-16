import { test, expect } from "@playwright/test";

// Runs against a fresh kima-rs container (empty /data): the first account becomes admin.
test("fresh install: health, register, log in, finish onboarding", async ({ page, request }) => {
    const consoleErrors: string[] = [];
    const failedApi: string[] = [];
    page.on("console", (m) => {
        if (m.type() === "error") consoleErrors.push(m.text());
    });
    page.on("response", (r) => {
        if (r.url().includes("/api/") && r.status() >= 400) {
            failedApi.push(`${r.status()} ${r.request().method()} ${r.url()}`);
        }
    });

    const health = await request.get("/health");
    expect(health.ok()).toBeTruthy();
    expect(await health.json()).toEqual({ status: "ok", checks: { db: "ok" } });

    const registered = await request.post("/api/onboarding/register", {
        data: { username: "smokeadmin", password: "smoke-pass-1" },
    });
    expect(registered.ok()).toBeTruthy();
    expect((await registered.json()).user.role).toBe("admin");

    await page.goto("/login/");
    await page.locator("#username").fill("smokeadmin");
    await page.locator("#password").fill("smoke-pass-1");
    await page.locator('button[type="submit"]').click();
    await expect(page).toHaveURL(/\/onboarding\/?$/);

    // Logged in but not onboarded: the page opens on the integrations step. Finishing with
    // every integration disabled lands on /sync, as upstream does.
    await page.getByRole("button", { name: "Save & Finish" }).click();
    await expect(page).toHaveURL(/\/sync\/?$/);
    const token = await page.evaluate(() => localStorage.getItem("auth_token"));
    const me = await request.get("/api/auth/me", {
        headers: { Authorization: `Bearer ${token}` },
    });
    expect((await me.json()).onboardingComplete).toBe(true);
    expect(failedApi.filter((f) => f.includes("/api/onboarding/"))).toEqual([]);
    console.log("CONSOLE_ERRORS", JSON.stringify(consoleErrors));
    console.log("FAILED_API", JSON.stringify(failedApi));
});

test("old album links land on the query-param page", async ({ page }) => {
    await page.goto("/album/some-id");
    await expect(page).toHaveURL(/\/album\/\?id=some-id$/);
});
