import { test, expect } from "@playwright/test";
// Stored SQL candidate only: KIMA_IDENTIFY=off; no search endpoint is ever called.
test("stored match: Database only apply then Undo restores stable album", async ({
    page,
    request,
}) => {
    const login = await request.post("/api/auth/login", {
        data: { username: "smokeadmin", password: "smoke-pass-1" },
    });
    expect(login.ok()).toBeTruthy();
    const session = await login.json();
    const headers = { Authorization: `Bearer ${session.token}` };
    const before = await (
        await request.get("/api/library/autotag/albums?q=The%20Woods", {
            headers,
        })
    ).json();
    const album = before.albums.find(
        (a: { album: string }) => a.album === "The Woods",
    );
    expect(album).toBeTruthy();
    await page.addInitScript((s) => {
        localStorage.setItem("auth_token", s.token);
        localStorage.setItem("refresh_token", s.refreshToken);
    }, session);
    const searches: string[] = [];
    page.on("request", (r) => {
        if (r.url().includes("/autotag/search")) searches.push(r.url());
    });
    await page.goto("/settings/");
    await page
        .getByRole("button", { name: "Library Tools", exact: true })
        .click();
    await page.getByRole("link", { name: "Match albums", exact: true }).click();
    await page
        .getByRole("link", { name: "Sleater-Kinney — The Woods", exact: true })
        .click();
    await expect(
        page.getByText("strong", { exact: true }).first(),
    ).toBeVisible();
    await page
        .getByRole("radio", { name: "Database only", exact: true })
        .check();
    const applied = page.waitForResponse(
        (r) =>
            r.url().includes(`/autotag/albums/${album.albumId}/apply`) &&
            r.request().method() === "POST",
    );
    await page.getByRole("button", { name: "Accept", exact: true }).click();
    const response = await applied;
    expect(response.status()).toBe(202);
    const { runId } = await response.json();
    await expect
        .poll(
            async () => {
                const r = await request.get(
                    `/api/library/autotag/runs/${runId}`,
                    {
                        headers,
                    },
                );
                return (await r.json()).state;
            },
            { timeout: 30000 },
        )
        .toBe("done");
    await page.goto(`/album/?id=${album.albumId}`);
    await expect(
        page.getByRole("heading", {
            name: "The Woods (Remastered)",
            exact: true,
        }),
    ).toBeVisible();
    await page.goto("/settings/autotag/");
    await page
        .getByRole("button", {
            name: /Sleater-Kinney — The Woods \(Remastered\): done/,
        })
        .click();
    await page.getByRole("button", { name: "Undo", exact: true }).click();
    await expect
        .poll(
            async () => {
                const r = await request.get(
                    `/api/library/autotag/runs/${runId}`,
                    {
                        headers,
                    },
                );
                return (await r.json()).state;
            },
            { timeout: 30000 },
        )
        .toBe("undone");
    await page.goto(`/album/?id=${album.albumId}`);
    await expect(
        page.getByRole("heading", { name: "The Woods", exact: true }),
    ).toBeVisible();
    expect(searches).toEqual([]);
});
