"use client";

import Link from "next/link";
import { usePathname } from "@/lib/use-pathname";
import { Library, Sparkles, BookOpen, Mic, ListMusic } from "lucide-react";
import { cn } from "@/lib/cn";
import { useIsMobile, useIsTablet } from "@/hooks/useMediaQuery";
import { useFeatures, type SectionFlag } from "@/lib/features-context";

const navigationItems: Array<{
    name: string;
    href: string;
    icon: typeof Library;
    matchPattern: string;
    feature?: SectionFlag;
}> = [
    {
        name: "Collection",
        href: "/collection",
        icon: Library,
        matchPattern: "/collection"
    },
    {
        name: "Vibe",
        href: "/vibe",
        icon: Sparkles,
        matchPattern: "/vibe"
    },
    {
        name: "Audiobooks",
        href: "/audiobooks",
        icon: BookOpen,
        matchPattern: "/audiobooks",
        feature: "audiobooks" as const,
    },
    {
        name: "Podcasts",
        href: "/podcasts",
        icon: Mic,
        matchPattern: "/podcasts",
        feature: "podcasts" as const,
    },
    {
        name: "Playlists",
        href: "/playlists",
        icon: ListMusic,
        matchPattern: "/playlist" // Matches both /playlists and /playlist/[id]
    },
];

export function BottomNavigation() {
    const pathname = usePathname();
    const features = useFeatures();
    const isMobile = useIsMobile();
    const isTablet = useIsTablet();
    const isMobileOrTablet = isMobile || isTablet;

    if (!isMobileOrTablet) return null;

    return (
        <nav
            className="fixed bottom-0 left-0 right-0 z-(--z-nav) bg-black border-t border-white/10"
            role="navigation"
            aria-label="Main navigation"
            style={{
                paddingBottom: 'var(--standalone-safe-area-bottom, 0px)'
            }}
        >
            <div className="flex items-center justify-around" style={{ height: "var(--nav-h)" }}>
                {navigationItems.filter((item) => !item.feature || features[item.feature]).map((item) => {
                    const isActive = pathname.startsWith(item.matchPattern);
                    const Icon = item.icon;

                    return (
                        <Link
                            key={item.name}
                            href={item.href}
                            className={cn(
                                "flex flex-col items-center justify-center flex-1 h-full py-2 transition-colors duration-200 landscape-short:flex-row landscape-short:gap-2 landscape-short:py-0",
                                isActive
                                    ? "text-white"
                                    : "text-[var(--text-muted)] active:text-gray-300"
                            )}
                            aria-label={item.name}
                            aria-current={isActive ? "page" : undefined}
                        >
                            <Icon
                                className={cn(
                                    "w-5 h-5 mb-1 transition-all duration-200 landscape-short:mb-0",
                                    isActive && "text-white"
                                )}
                                strokeWidth={isActive ? 2.5 : 2}
                            />
                            <span
                                className={cn(
                                    "text-micro tracking-wide transition-colors duration-200",
                                    isActive ? "font-semibold" : "font-medium"
                                )}
                            >
                                {item.name}
                            </span>
                        </Link>
                    );
                })}
            </div>
        </nav>
    );
}
