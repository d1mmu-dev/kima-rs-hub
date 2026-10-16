import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

interface PageHeaderProps {
    eyebrow?: string;
    title: ReactNode;
    description?: ReactNode;
    actions?: ReactNode;
    children?: ReactNode;
    className?: string;
}

export function PageHeader({ eyebrow, title, description, actions, children, className }: PageHeaderProps) {
    return (
        <header className={cn("page-shell pt-8 pb-6", className)}>
            {eyebrow && (
                <p className="t-eyebrow mb-4 flex items-center gap-2">
                    <span aria-hidden="true" className="w-1.5 h-1.5 rounded-full bg-brand" />
                    {eyebrow}
                </p>
            )}
            <div className="flex flex-wrap items-end justify-between gap-4">
                <div className="min-w-0">
                    <h1 className="text-4xl md:text-5xl font-bold tracking-tight text-white leading-none">{title}</h1>
                    {description && <p className="mt-3 text-sm text-[var(--text-muted)]">{description}</p>}
                </div>
                {actions && <div className="flex flex-wrap items-center gap-3">{actions}</div>}
            </div>
            {children}
        </header>
    );
}
