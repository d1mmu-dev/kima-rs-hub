"use client";

import { createContext, useContext, useEffect, useState, useMemo, ReactNode } from "react";
import { api } from "./api";
import { useAuth } from "./auth-context";
import { SectionFlagsContext, type SectionFlag, type SectionFlags } from "./section-flags";

export type { SectionFlag } from "./section-flags";
export { useSectionFlags } from "./section-flags";

interface FeaturesState {
    musicCNN: boolean;
    vibeEmbeddings: boolean;
    audiobookshelfEnabled: boolean;
    /** This user's player reports what it plays for the public live feed (the owner's own instance only). */
    liveReporting: boolean;
    /** Which app sections exist on this server (D-030); a section whose flag is false is hidden. */
    library: boolean;
    podcasts: boolean;
    audiobooks: boolean;
    discover: boolean;
    mixes: boolean;
    downloads: boolean;
    imports: boolean;
    /** Listening stats (`/stats/*`, M-STATS) and the release changelog (`/whats-new/*`); off until kima-rs serves them. */
    stats: boolean;
    changelog: boolean;
    loading: boolean;
}

interface Fetched extends Omit<FeaturesState, "liveReporting"> {
    /** The user the server last said reports live listening, so nobody else signing in inherits it. */
    liveReportingUser: string | null;
}

const initial: Fetched = {
    musicCNN: false,
    vibeEmbeddings: false,
    audiobookshelfEnabled: false,
    liveReportingUser: null,
    library: true,
    podcasts: false,
    audiobooks: false,
    discover: false,
    mixes: false,
    downloads: false,
    imports: false,
    stats: false,
    changelog: false,
    loading: true,
};

const FeaturesContext = createContext<FeaturesState | undefined>(undefined);

export function FeaturesProvider({ children }: { children: ReactNode }) {
    const { isAuthenticated, user } = useAuth();
    const userId = user?.id ?? null;
    const [state, setState] = useState<Fetched>(initial);

    useEffect(() => {
        if (!isAuthenticated || !userId) return;
        // A reply that lands after a sign-out or a switch of user belongs to the previous one.
        let current = true;
        // After a check that worked, a failure is retried in 5 s rather than at the next minute,
        // so the owner's listening is not left unreported; repeated failures wait for the minute.
        let lastFailed = false;
        let retry: ReturnType<typeof setTimeout> | null = null;

        const fetchFeatures = () => {
            api.getFeatures()
                .then((features) => {
                    if (!current) return;
                    lastFailed = false;
                    setState({
                        musicCNN: features.musicCNN,
                        vibeEmbeddings: features.vibeEmbeddings,
                        audiobookshelfEnabled: features.audiobookshelfEnabled,
                        liveReportingUser: features.liveReporting === true ? userId : null,
                        // A key the server does not send is a section it does not have (D-030).
                        library: features.library ?? true,
                        podcasts: features.podcasts ?? false,
                        audiobooks: features.audiobooks ?? false,
                        discover: features.discover ?? false,
                        mixes: features.mixes ?? false,
                        downloads: features.downloads ?? false,
                        imports: features.imports ?? false,
                        stats: features.stats ?? false,
                        changelog: features.changelog ?? false,
                        loading: false,
                    });
                })
                .catch((error) => {
                    if (!current) return;
                    console.error("Failed to fetch features:", error);
                    setState((prev) => ({ ...prev, loading: false }));
                    if (!lastFailed && !retry) {
                        retry = setTimeout(() => {
                            retry = null;
                            fetchFeatures();
                        }, 5_000);
                    }
                    lastFailed = true;
                });
        };

        fetchFeatures();
        const interval = setInterval(fetchFeatures, 60000);
        return () => {
            current = false;
            clearInterval(interval);
            if (retry) clearTimeout(retry);
        };
    }, [isAuthenticated, userId]);

    const value = useMemo<FeaturesState>(
        () => ({
            musicCNN: state.musicCNN,
            vibeEmbeddings: state.vibeEmbeddings,
            audiobookshelfEnabled: state.audiobookshelfEnabled,
            liveReporting: isAuthenticated && userId !== null && state.liveReportingUser === userId,
            library: state.library,
            podcasts: state.podcasts,
            audiobooks: state.audiobooks,
            discover: state.discover,
            mixes: state.mixes,
            downloads: state.downloads,
            imports: state.imports,
            stats: state.stats,
            changelog: state.changelog,
            loading: state.loading,
        }),
        [state, isAuthenticated, userId],
    );

    const sections = useMemo<SectionFlags>(
        () => ({
            podcasts: value.podcasts,
            audiobooks: value.audiobooks,
            discover: value.discover,
            mixes: value.mixes,
            downloads: value.downloads,
            imports: value.imports,
            stats: value.stats,
            changelog: value.changelog,
        }),
        [value],
    );

    return (
        <FeaturesContext.Provider value={value}>
            <SectionFlagsContext.Provider value={sections}>{children}</SectionFlagsContext.Provider>
        </FeaturesContext.Provider>
    );
}

export function useFeatures(): FeaturesState {
    const context = useContext(FeaturesContext);
    if (!context) {
        throw new Error("useFeatures must be used within FeaturesProvider");
    }
    return context;
}
