"use client";

import Link from "next/link";
import { SettingsSection } from "../ui";
import { buttonClass } from "@/components/ui/Button";

/** Entry points to the admin-only organizer and attention report. */
export function LibraryToolsSection() {
    return (
        <SettingsSection
            id="library-tools"
            title="Library Tools"
            description="Tidy the on-disk layout and see what the scanner had to guess"
        >
            <div className="flex flex-wrap gap-3 py-2">
                <Link href="/settings/organize" className={buttonClass("secondary", "md")}>
                    Organize library
                </Link>
                <Link href="/settings/attention" className={buttonClass("secondary", "md")}>
                    Needs attention
                </Link>
            </div>
        </SettingsSection>
    );
}
