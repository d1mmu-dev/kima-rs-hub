import type { Metadata, Viewport } from "next";
import localFont from "next/font/local";
import "./globals.css";
import { AuthProvider } from "@/lib/auth-context";
import { FeaturesProvider } from "@/lib/features-context";
import { DownloadProvider } from "@/lib/download-context";
import { ConditionalAudioProvider } from "@/components/providers/ConditionalAudioProvider";
import { AuthenticatedLayout } from "@/components/layout/AuthenticatedLayout";
import { QueryProvider } from "@/lib/query-client";
import { ServiceWorkerRegistration } from "@/components/ServiceWorkerRegistration";
import { GlobalErrorBoundary } from "@/components/providers/GlobalErrorBoundary";
import { ActivityPanelSettingsProvider } from "@/lib/activity-panel-settings-context";

const montserrat = localFont({
    src: "../public/fonts/montserrat-latin.woff2",
    variable: "--font-montserrat",
    display: "swap",
    weight: "300 800",
});

export const viewport: Viewport = {
    width: "device-width",
    initialScale: 1,
    maximumScale: 1,
    userScalable: false,
    viewportFit: "cover",
    themeColor: "#000000",
};

export const metadata: Metadata = {
    title: "kima",
    description: "self-hosted music streaming. https://github.com/Chevron7Locked/kima-hub",
    manifest: "/manifest.webmanifest",
    icons: {
        apple: [
            { url: "/assets/images/apple-touch-icon.png", sizes: "180x180", type: "image/png" },
        ],
    },
    appleWebApp: {
        // Standalone PWA on iOS so "Add to Home Screen" opens fullscreen and
        // safe-area-inset values activate, freeing the top nav and bottom
        // player from Safari chrome overlap.
        capable: true,
        statusBarStyle: "black-translucent",
        title: "kima",
    },
    openGraph: {
        title: "kima",
        description: "self-hosted music streaming",
        siteName: "kima",
        url: "https://github.com/Chevron7Locked/kima-hub",
        type: "website",
    },
};

export default function RootLayout({
    children,
}: Readonly<{
    children: React.ReactNode;
}>) {
    return (
        <html lang="en">
            <body className={`${montserrat.variable} antialiased`}>
                <GlobalErrorBoundary>
                    <ServiceWorkerRegistration />
                    <AuthProvider>
                        <FeaturesProvider>
                            <QueryProvider>
                                <DownloadProvider>
                                    <ConditionalAudioProvider>
                                        <ActivityPanelSettingsProvider>
                                            <AuthenticatedLayout>
                                                {children}
                                            </AuthenticatedLayout>
                                        </ActivityPanelSettingsProvider>
                                    </ConditionalAudioProvider>
                                </DownloadProvider>
                            </QueryProvider>
                        </FeaturesProvider>
                    </AuthProvider>
                </GlobalErrorBoundary>
            </body>
        </html>
    );
}
