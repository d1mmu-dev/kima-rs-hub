import { test, expect, type APIRequestContext } from "@playwright/test";

// Runs after smoke.spec.ts against the same kima; also works alone on a fresh install.
const USER = { username: "smokeadmin", password: "smoke-pass-1" };

async function session(request: APIRequestContext) {
    let res = await request.post("/api/auth/login", { data: USER });
    if (!res.ok()) {
        const reg = await request.post("/api/onboarding/register", { data: USER });
        expect(reg.ok()).toBeTruthy();
        res = await request.post("/api/auth/login", { data: USER });
    }
    expect(res.ok()).toBeTruthy();
    const body = await res.json();
    await request.post("/api/onboarding/complete", {
        headers: { Authorization: `Bearer ${body.token}` },
    });
    return { token: body.token as string, refreshToken: body.refreshToken as string };
}

test("native apps: create a token, use it on /rest, revoke it", async ({ page, request }) => {
    const s = await session(request);
    await page.addInitScript((v) => {
        localStorage.setItem("auth_token", v.token);
        localStorage.setItem("refresh_token", v.refreshToken);
    }, s);

    await page.goto("/settings");
    await page.getByRole("button", { name: /native apps/i }).click();
    await expect(page.getByText("Setup Guide")).toBeVisible();
    await expect(page.getByText("Active Tokens")).toBeVisible();

    await page.getByPlaceholder(/Client name/).fill("e2e");
    await page.getByRole("button", { name: "Generate Token" }).click();

    // Shown once: a 64-character hex token in the box.
    const box = page.locator("code").filter({ hasText: /^[0-9a-f]{64}$/ });
    await expect(box).toBeVisible();
    const token = (await box.textContent())!.trim();
    await expect(page.getByText("Never used")).toBeVisible();
    await expect(page.getByText("e2e", { exact: true })).toBeVisible();

    const ping = (tok: string) =>
        page.evaluate(async ([u, p]) => {
            const r = await fetch(`/rest/ping.view?u=${encodeURIComponent(u)}&p=${p}&f=json`);
            return (await r.json())["subsonic-response"].status as string;
        }, [USER.username, tok]);

    expect(await ping(token)).toBe("ok");

    const row = page
        .locator("div.flex.items-center.justify-between")
        .filter({ has: page.getByText("e2e", { exact: true }) });
    await row.getByRole("button", { name: "Revoke" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Revoke" }).click();
    await expect(page.getByText("No tokens yet.")).toBeVisible();

    expect(await ping(token)).toBe("failed");
});
