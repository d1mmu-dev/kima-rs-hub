import type { NextConfig } from "next";

const isDev = process.env.NODE_ENV === "development";
const devBackend = process.env.NEXT_PUBLIC_BACKEND_URL || "http://127.0.0.1:3111";

const nextConfig: NextConfig = {
    turbopack: {
        root: __dirname,
    },
    
    // Dev-server origins. Add your own (e.g. a LAN/tailnet address you browse
    // from) via KIMA_DEV_ORIGINS="host1,http://host2:3030" — never hard-coded here.
    allowedDevOrigins: [
        "http://127.0.0.1:3030",
        "http://127.0.0.1",
        "127.0.0.1",
        "http://localhost:3030",
        "http://localhost",
        "localhost",
        ...(process.env.KIMA_DEV_ORIGINS ?? "").split(",").map((s) => s.trim()).filter(Boolean),
    ],
    
    images: {
        unoptimized: true,
        remotePatterns: [
            {
                protocol: "https",
                hostname: "cdn-images.dzcdn.net",
                pathname: "/**",
            },
            {
                protocol: "https",
                hostname: "e-cdns-images.dzcdn.net",
                pathname: "/**",
            },
            {
                protocol: "http",
                hostname: "localhost",
                port: "3006",
                pathname: "/**",
            },
            {
                protocol: "http",
                hostname: "127.0.0.1",
                port: "3006",
                pathname: "/**",
            },
            {
                protocol: "https",
                hostname: "assets.pippa.io",
                pathname: "/**",
            },
            {
                protocol: "https",
                hostname: "is1-ssl.mzstatic.com",
                pathname: "/**",
            },
        ],
        formats: ["image/avif", "image/webp"],
        deviceSizes: [640, 750, 828, 1080, 1200, 1920, 2048, 3840],
        imageSizes: [16, 32, 48, 64, 96, 128, 256, 384],
        minimumCacheTTL: 60 * 60 * 24 * 7, // Cache for 7 days
        // SVG optimization disabled for security (prevents XSS via crafted SVGs)
    },
    reactStrictMode: true,
    trailingSlash: true,
    ...(isDev
        ? {
              // Next's trailing-slash redirect runs before rewrites and would send /api/x to
              // /api/x/, which the rewrites below don't match. The export is unaffected.
              skipTrailingSlashRedirect: true,
              async rewrites() {
                  return [
                      { source: "/api/:path*", destination: `${devBackend}/api/:path*` },
                      { source: "/rest/:path*", destination: `${devBackend}/rest/:path*` },
                      { source: "/health", destination: `${devBackend}/health` },
                  ];
              },
          }
        : { output: "export" as const }),
};

// The bundle analyzer is a devDependency, stripped from production images.
// Next awaits a promise-returning config export, so load it lazily: prod
// boots never reference the package. ANALYZE=true (dev) gets the report.
export default async function config(): Promise<NextConfig> {
    if (process.env.ANALYZE === "true") {
        const { default: withBundleAnalyzer } = await import("@next/bundle-analyzer");
        return withBundleAnalyzer({ enabled: true })(nextConfig);
    }
    return nextConfig;
}
