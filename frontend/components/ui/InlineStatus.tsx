"use client";

import { useState, useEffect } from "react";
import { Check, X, Loader2 } from "lucide-react";
import { cn } from "@/lib/cn";

export type StatusType = "idle" | "loading" | "success" | "error";

interface InlineStatusProps {
    status: StatusType;
    message?: string;
    className?: string;
    showIcon?: boolean;
    autoClear?: boolean; // Auto-clear success/error after delay
    clearDelay?: number; // Delay in ms before clearing (default 3000)
    onClear?: () => void;
}

/**
 * Inline status indicator for form fields and buttons
 * Shows success/error/loading states without overlay toasts
 */
export function InlineStatus({
    status,
    message,
    className,
    showIcon = true,
    autoClear = true,
    clearDelay = 3000,
    onClear,
}: InlineStatusProps) {
    const [visible, setVisible] = useState(status !== "idle");

    // Sync visibility with status changes during render
    const [prevStatus, setPrevStatus] = useState(status);
    if (status !== prevStatus) {
        setPrevStatus(status);
        setVisible(status !== "idle");
    }

    useEffect(() => {
        if ((status === "success" || status === "error") && autoClear) {
            const timer = setTimeout(() => {
                setVisible(false);
                onClear?.();
            }, clearDelay);
            return () => clearTimeout(timer);
        }
    }, [status, autoClear, clearDelay, onClear]);

    if (status === "idle" || !visible) return null;

    return (
        <span
            className={cn(
                "inline-flex items-center gap-1.5 text-sm",
                status === "success" && "text-success",
                status === "error" && "text-danger-light",
                status === "loading" && "text-[var(--text-secondary)]",
                className
            )}
            aria-live="polite"
        >
            {showIcon && (
                <>
                    {status === "success" && <Check className="w-4 h-4" />}
                    {status === "error" && <X className="w-4 h-4" />}
                    {status === "loading" && <Loader2 className="w-4 h-4 animate-spin" />}
                </>
            )}
            {message && <span>{message}</span>}
        </span>
    );
}

export function useInlineStatus(initialStatus: StatusType = "idle") {
    const [status, setStatus] = useState<StatusType>(initialStatus);
    const [message, setMessage] = useState<string>("");

    const setSuccess = (msg?: string) => {
        setStatus("success");
        setMessage(msg || "");
    };

    const setError = (msg?: string) => {
        setStatus("error");
        setMessage(msg || "");
    };

    const setLoading = (msg?: string) => {
        setStatus("loading");
        setMessage(msg || "");
    };

    const reset = () => {
        setStatus("idle");
        setMessage("");
    };

    return {
        status,
        message,
        setSuccess,
        setError,
        setLoading,
        reset,
        props: { status, message, onClear: reset },
    };
}

