import { defineConfig } from "vitest/config";
import path from "path";

export default defineConfig({
    test: {
        environment: "happy-dom",
        // Node 25+ ships its own `localStorage` global (empty without --localstorage-file), which hides
        // happy-dom's; the comparison reference runs on the host Node, so the workers turn it off.
        execArgv: ["--no-experimental-webstorage"],
        include: [
            "lib/__tests__/**/*.test.{ts,tsx}",
            "hooks/__tests__/**/*.test.{ts,tsx}",
            "features/**/__tests__/**/*.test.{ts,tsx}",
            "components/**/__tests__/**/*.test.{ts,tsx}",
        ],
    },
    resolve: {
        alias: {
            "@": path.resolve(__dirname, "."),
        },
    },
});
