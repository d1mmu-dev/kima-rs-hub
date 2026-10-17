import { test, expect, type APIRequestContext, type Page } from "@playwright/test";

// Runs after smoke.spec.ts against the same kima (fixture library from tools/make-fixture-library.sh).
const USER = { username: "smokeadmin", password: "smoke-pass-1" };

interface Session {
    token: string;
    refreshToken: string;
}

async function session(request: APIRequestContext): Promise<Session> {
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
    return { token: body.token, refreshToken: body.refreshToken };
}

async function signIn(page: Page, s: Session) {
    await page.addInitScript((v) => {
        localStorage.setItem("auth_token", v.token);
        localStorage.setItem("refresh_token", v.refreshToken);
    }, s);
}

const auth = (s: Session) => ({ Authorization: `Bearer ${s.token}` });

interface AlbumRow {
    id: string;
    title: string;
    artistId: string;
    trackCount: number;
}

// Fixture tracks: 3 + 6 + 3 + 2 = 14. The startup scan may still be running, so wait for all of them.
async function waitForLibrary(request: APIRequestContext, s: Session): Promise<AlbumRow[]> {
    await request.post("/api/library/scan", { headers: auth(s) });
    let albums: AlbumRow[] = [];
    await expect
        .poll(
            async () => {
                const res = await request.get("/api/library/albums?limit=100", { headers: auth(s) });
                albums = (await res.json()).albums ?? [];
                return albums.reduce((n, a) => n + a.trackCount, 0);
            },
            { timeout: 60_000, intervals: [500, 1000, 2000] },
        )
        .toBe(14);
    return albums;
}

function album(albums: AlbumRow[], title: string): AlbumRow {
    const a = albums.find((x) => x.title === title);
    expect(a, `album ${title}`).toBeTruthy();
    return a!;
}

async function currentTime(page: Page): Promise<number> {
    return page.evaluate(
        () => document.querySelector<HTMLAudioElement>('audio[data-kima-player="main"]')?.currentTime ?? 0,
    );
}

test("scan, browse, play", async ({ page, request }) => {
    const s = await session(request);
    const albums = await waitForLibrary(request, s);
    await signIn(page, s);

    // Folder-derived metadata for the untagged album, the multi-disc album and the hyphenated artist.
    const plain = album(albums, "Plain Album");
    expect(album(albums, "The Woods").trackCount).toBe(2);
    expect(album(albums, "Double Album").trackCount).toBe(6);

    await page.goto(`/artist/?id=${plain.artistId}`);
    await expect(page.getByText("Untagged Artist").first()).toBeVisible();
    await page.locator('a[href*="/album/?id="]').filter({ hasText: "Plain Album" }).first().click();
    await expect(page).toHaveURL(new RegExp(`/album/\\?id=${plain.id}`));
    await expect(page.getByText("Second").first()).toBeVisible();

    await page.getByRole("button", { name: "play all" }).click();
    await expect.poll(() => currentTime(page), { timeout: 5_000 }).toBeGreaterThan(0);
});

test("seek works", async ({ page, request }) => {
    const s = await session(request);
    const albums = await waitForLibrary(request, s);
    await signIn(page, s);
    const plain = album(albums, "Plain Album");

    // Start byte of every 206 /stream response.
    const ranged: number[] = [];
    page.on("response", (r) => {
        if (!r.url().includes("/stream") || r.status() !== 206) return;
        const m = /bytes (\d+)-/.exec(r.headers()["content-range"] ?? "");
        if (m) ranged.push(Number(m[1]));
    });

    await page.goto(`/album/?id=${plain.id}`);
    await page.getByRole("button", { name: "play all" }).click();
    await expect.poll(() => currentTime(page), { timeout: 5_000 }).toBeGreaterThan(0);

    // Only requests caused by the seek count.
    ranged.length = 0;
    await page.evaluate(() => {
        const a = document.querySelector<HTMLAudioElement>('audio[data-kima-player="main"]')!;
        a.currentTime = 450;
    });
    // Playback continues from the seek target, not from the start.
    await expect.poll(() => currentTime(page), { timeout: 5_000 }).toBeGreaterThan(450);
    // The seek made a ranged request that starts past byte 0.
    await expect.poll(() => ranged.some((start) => start > 0), { timeout: 5_000 }).toBe(true);
});

test("playlist create and add", async ({ page, request }) => {
    const s = await session(request);
    const albums = await waitForLibrary(request, s);
    await signIn(page, s);
    const woods = album(albums, "The Woods");

    await page.goto(`/album/?id=${woods.id}`);
    await page.getByRole("button", { name: "add to playlist" }).first().click();
    await page.getByPlaceholder("Enter playlist name...").fill("E2E mix");
    await page.getByRole("button", { name: "Create", exact: true }).click();

    await expect
        .poll(
            async () => {
                const res = await request.get("/api/playlists", { headers: auth(s) });
                const list: { id: string; name: string }[] = await res.json();
                const p = list.find((x) => x.name === "E2E mix");
                if (!p) return -1;
                const d = await (await request.get(`/api/playlists/${p.id}`, { headers: auth(s) })).json();
                return (d.items as unknown[]).length;
            },
            { timeout: 10_000 },
        )
        .toBeGreaterThan(0);
});

test("organizer preview lists moves", async ({ page, request }) => {
    const s = await session(request);
    await waitForLibrary(request, s);
    await signIn(page, s);

    await page.goto("/settings/organize/");
    await expect(page.getByText(/Copy mode:\s*available/)).toBeVisible();
    await page.getByRole("button", { name: /preview/i }).first().click();
    await expect(page.getByText(/Disc Band\/2004 - Double Album\/1-01 - Song 1-1\.mp3/)).toBeVisible({
        timeout: 15_000,
    });
});

test("same-route navigation resets state", async ({ page, request }) => {
    const s = await session(request);
    const albums = await waitForLibrary(request, s);
    await signIn(page, s);
    const woods = album(albums, "The Woods");
    const plain = album(albums, "Plain Album");

    await page.goto(`/album/?id=${plain.id}`);
    await expect(page.getByText("Third").first()).toBeVisible();

    // Client-side navigation through Next's router to another album on the same /album/ route.
    await page.evaluate(() => {
        (window as unknown as { __noReload: boolean }).__noReload = true;
    });
    await page.evaluate((id) => {
        const w = window as unknown as { next: { router: { push: (u: string) => void } } };
        w.next.router.push(`/album/?id=${id}`);
    }, woods.id);
    await expect(page.getByText("Woods 2").first()).toBeVisible();
    await expect(page.getByText("Third")).toHaveCount(0);
    expect(await page.evaluate(() => (window as unknown as { __noReload?: boolean }).__noReload)).toBe(true);
});

test("hidden sections are hidden", async ({ page, request }) => {
    const s = await session(request);
    await signIn(page, s);
    const features = page.waitForResponse((r) => r.url().includes("/api/system/features"));
    await page.goto("/");
    expect((await features).ok()).toBe(true);
    // "Collection" is always shown; Radio and Discovery need the flags (mixes and discover are
    // off), so their absence is only meaningful once the flags have been applied.
    await expect(page.getByRole("link", { name: "Collection" }).first()).toBeVisible();
    await expect(page.getByRole("link", { name: "Radio" })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "Discovery" })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "Podcasts" })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "Audiobooks" })).toHaveCount(0);
});
