import type { Metadata } from "next";

export const metadata: Metadata = {
    title: "shared music - kima",
    description: "listen to shared music on kima. self-hosted music streaming. https://github.com/Chevron7Locked/kima-hub",
    openGraph: {
        title: "shared music - kima",
        description: "listen to shared music on kima, self-hosted music streaming.",
        siteName: "kima",
        url: "https://github.com/Chevron7Locked/kima-hub",
        type: "music.song",
    },
};

export default function ShareLayout({ children }: { children: React.ReactNode }) {
    return children;
}
