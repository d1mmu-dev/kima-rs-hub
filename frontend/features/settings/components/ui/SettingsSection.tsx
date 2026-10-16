import { ReactNode } from "react";
import { SectionHeader } from "@/components/ui/SectionHeader";

interface SettingsSectionProps {
    id: string;
    title: string;
    description?: string;
    children: ReactNode;
    showSeparator?: boolean;
}

export function SettingsSection({
    id,
    title,
    description,
    children,
    showSeparator = true
}: SettingsSectionProps) {
    return (
        <section id={id} className="scroll-mt-24 animate-rise">
            <div className="mb-4">
                <SectionHeader title={title} size="sm" className="mb-1" />
                {description && (
                    <p className="text-xs tabular-nums text-[var(--text-muted)] mt-1 ml-[calc(0.25rem+0.75rem+4px)]">{description}</p>
                )}
            </div>

            <div className="space-y-1">
                {children}
            </div>

            {showSeparator && (
                <div className="border-t border-white/5 mt-8 mb-8" />
            )}
        </section>
    );
}
