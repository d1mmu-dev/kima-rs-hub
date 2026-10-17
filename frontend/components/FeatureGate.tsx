"use client";

import type { ReactNode } from "react";
import { useFeatures, type SectionFlag } from "@/lib/features-context";
import { LoadingScreen } from "@/components/ui/LoadingScreen";
import { NotAvailable } from "@/features/organizer/components/NotAvailable";

/**
 * Renders its page only when the server has the section (D-030). A direct visit to a
 * section that is not ported yet shows a small panel instead of a page full of 404s.
 */
export function FeatureGate({ flag, children }: { flag: SectionFlag; children: ReactNode }) {
    const features = useFeatures();
    if (features.loading) return <LoadingScreen />;
    if (!features[flag]) return <NotAvailable />;
    return <>{children}</>;
}
