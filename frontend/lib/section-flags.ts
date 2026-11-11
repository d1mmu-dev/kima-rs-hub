"use client";

import { createContext, useContext } from "react";

/** App sections a server may lack (D-030): `/system/features` says which exist on this one. */
export type SectionFlag =
    | "podcasts"
    | "audiobooks"
    | "discover"
    | "mixes"
    | "downloads"
    | "imports"
    | "stats"
    | "changelog"
    | "accountDeletion";

export type SectionFlags = Record<SectionFlag, boolean>;

const ALL_SECTIONS: SectionFlags = {
    podcasts: true,
    audiobooks: true,
    discover: true,
    mixes: true,
    downloads: true,
    imports: true,
    stats: true,
    changelog: true,
    accountDeletion: true,
};

/**
 * Filled by FeaturesProvider. Its own module so the upstream component tests, which mock
 * `@/lib/features-context` without knowing this hook, still render: outside the provider every
 * section shows, as it does upstream.
 */
export const SectionFlagsContext = createContext<SectionFlags>(ALL_SECTIONS);

/** The section flags the navigation, the activity panel and the home rows hide by. */
export function useSectionFlags(): SectionFlags {
    return useContext(SectionFlagsContext);
}
