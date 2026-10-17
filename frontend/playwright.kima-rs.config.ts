import { defineConfig } from "@playwright/test";

// The smoke project registers the first admin on a fresh install; the library project
// then logs in as that admin, so it runs after smoke.
export default defineConfig({
    testDir: "./tests/e2e/kima-rs",
    forbidOnly: !!process.env.CI,
    use: {
        baseURL: process.env.KIMA_URL ?? "http://localhost:3030",
        trace: "retain-on-failure",
        screenshot: "only-on-failure",
    },
    reporter: "list",
    workers: 1,
    projects: [
        { name: "smoke", testMatch: "smoke.spec.ts" },
        { name: "library", testMatch: "library.spec.ts", dependencies: ["smoke"] },
    ],
});
