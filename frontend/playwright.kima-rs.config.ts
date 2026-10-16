import { defineConfig } from "@playwright/test";

export default defineConfig({
    testDir: "./tests/e2e/kima-rs",
    use: { baseURL: process.env.KIMA_URL ?? "http://localhost:3030" },
    reporter: "list",
});
