"use client";

import { ReactNode, useId, useRef } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/cn";
import { Button } from "./Button";
import { useDialog } from "@/hooks/useDialog";

export interface ModalProps {
    isOpen: boolean;
    onClose: () => void;
    title: string;
    children: ReactNode;
    footer?: ReactNode;
    className?: string;
}

export function Modal({
    isOpen,
    onClose,
    title,
    children,
    footer,
    className,
}: ModalProps) {
    const dialogRef = useRef<HTMLDivElement>(null);
    const titleId = useId();
    useDialog(dialogRef, isOpen, onClose);

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-(--z-modal) flex items-center justify-center p-4 bg-black/60 transition-opacity duration-200"
            onClick={onClose}
        >
            <div
                ref={dialogRef}
                role="dialog"
                aria-modal="true"
                aria-labelledby={titleId}
                onClick={(e) => e.stopPropagation()}
                className={cn(
                    "bg-gradient-to-br from-[#141414] to-[#0f0f0f] border border-[var(--border-interactive)] rounded-xl max-w-md w-full p-6 animate-pop",
                    className
                )}
            >
                <div className="flex items-center justify-between mb-4 pb-4 border-b border-[var(--border-subtle)]">
                    <h2 id={titleId} className="text-lg font-medium text-white">{title}</h2>
                    <Button
                        variant="icon"
                        onClick={onClose}
                        aria-label="Close"
                    >
                        <X className="w-5 h-5" />
                    </Button>
                </div>

                <div className="mb-6">{children}</div>

                {footer && (
                    <div className="flex gap-3 justify-end">{footer}</div>
                )}
            </div>
        </div>
    );
}
