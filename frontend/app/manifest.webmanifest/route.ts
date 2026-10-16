import { NextResponse } from "next/server";

// Standalone on every platform; the audio controller's AudioContext bridge
// keeps the iOS audio session alive.

const MANIFEST = {
    id: "/",
    name: "kima",
    short_name: "kima",
    description: "self-hosted music streaming",
    lang: "en",
    scope: "/",
    start_url: "/",
    orientation: "portrait",
    theme_color: "#000000",
    background_color: "#000000",
    categories: ["music", "entertainment"],
    icons: [
        { src: "assets/icons/icon-48.webp", type: "image/webp", sizes: "48x48", purpose: "any" },
        { src: "assets/icons/icon-72.webp", type: "image/webp", sizes: "72x72", purpose: "any" },
        { src: "assets/icons/icon-96.webp", type: "image/webp", sizes: "96x96", purpose: "any" },
        { src: "assets/icons/icon-128.webp", type: "image/webp", sizes: "128x128", purpose: "any" },
        { src: "assets/icons/icon-192.webp", type: "image/webp", sizes: "192x192", purpose: "any maskable" },
        { src: "assets/icons/icon-256.webp", type: "image/webp", sizes: "256x256", purpose: "any" },
        { src: "assets/icons/icon-512.webp", type: "image/webp", sizes: "512x512", purpose: "any maskable" },
    ],
    shortcuts: [
        { name: "vibe", short_name: "vibe", url: "/vibe", icons: [{ src: "assets/icons/icon-96.webp", sizes: "96x96" }] },
        { name: "search", short_name: "search", url: "/search", icons: [{ src: "assets/icons/icon-96.webp", sizes: "96x96" }] },
        { name: "library", short_name: "library", url: "/library", icons: [{ src: "assets/icons/icon-96.webp", sizes: "96x96" }] },
    ],
    display: "standalone",
    display_override: ["window-controls-overlay", "standalone"],
};

export async function GET() {
    return NextResponse.json(MANIFEST, {
        headers: {
            "Content-Type": "application/manifest+json",
            "Cache-Control": "public, max-age=3600",
        },
    });
}
