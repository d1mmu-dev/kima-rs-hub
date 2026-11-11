import {
    BookAudio,
    CalendarClock,
    ChartNoAxesColumn,
    Compass,
    Home,
    LayoutGrid,
    Library,
    ListMusic,
    Orbit,
    Podcast,
    Radio,
    Search,
    type LucideIcon,
} from "lucide-react";
import type { SectionFlag } from "@/lib/features-context";

export interface NavItem {
    name: string;
    href: string;
    icon: LucideIcon;
    /** Routes that belong to this destination, so a detail page keeps its parent lit. */
    match: string[];
    /** The `/system/features` section the destination needs; hidden when the server lacks it (D-030). */
    feature?: SectionFlag;
}

/** Every place the app's navigation leads; each surface (rail, tab bar, phone menu, TV bar) picks its own. */
export const NAV = {
    home: { name: "home", href: "/", icon: Home, match: ["/"] },
    search: { name: "search", href: "/search", icon: Search, match: ["/search"] },
    collection: {
        name: "collection",
        href: "/collection",
        icon: Library,
        match: ["/collection", "/library", "/album", "/artist", "/mix"],
    },
    radio: { name: "radio", href: "/radio", icon: Radio, match: ["/radio"], feature: "mixes" },
    discover: { name: "discovery", href: "/discover", icon: Compass, match: ["/discover"], feature: "discover" },
    releases: { name: "releases", href: "/releases", icon: CalendarClock, match: ["/releases"] },
    vibe: { name: "vibe", href: "/vibe", icon: Orbit, match: ["/vibe"] },
    audiobooks: {
        name: "audiobooks",
        href: "/audiobooks",
        icon: BookAudio,
        match: ["/audiobooks"],
        feature: "audiobooks",
    },
    podcasts: { name: "podcasts", href: "/podcasts", icon: Podcast, match: ["/podcasts"], feature: "podcasts" },
    browse: { name: "browse", href: "/browse/playlists", icon: LayoutGrid, match: ["/browse", "/playlists"] },
    playlists: { name: "playlists", href: "/playlists", icon: ListMusic, match: ["/playlists", "/playlist"] },
    stats: { name: "stats", href: "/stats", icon: ChartNoAxesColumn, match: ["/stats"], feature: "stats" },
} satisfies Record<string, NavItem>;

/** Whether `pathname` is the item's page or one beneath it. */
export const isCurrentNav = (item: NavItem, pathname: string) =>
    item.match.some((path) => pathname === path || (path !== "/" && pathname.startsWith(`${path}/`)));
