import { ButtonHTMLAttributes, forwardRef, memo } from "react";
import { cn } from "@/lib/cn";
import { GradientSpinner } from "./GradientSpinner";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
    variant?: "primary" | "secondary" | "ghost" | "danger" | "ai" | "icon";
    size?: "sm" | "md" | "lg";
    isLoading?: boolean;
}

const BASE =
    "inline-flex items-center justify-center gap-2 min-h-[44px] rounded-lg font-semibold transition-colors duration-200 disabled:opacity-50 disabled:cursor-not-allowed";

const VARIANT = {
    primary: "bg-brand hover:bg-brand-hover text-black",
    secondary: "bg-white/5 hover:bg-white/10 text-[var(--text-secondary)] hover:text-white border border-white/10 hover:border-white/20",
    ghost: "text-[var(--text-secondary)] hover:text-white hover:bg-[var(--bg-hover)]",
    danger: "text-danger hover:bg-danger/10 border border-danger/20 hover:border-danger/40",
    ai: "bg-[var(--bg-hover)] hover:bg-brand/10 text-brand border border-[var(--border-subtle)] hover:border-brand/30",
    icon: "w-11 h-11 text-[var(--text-secondary)] hover:text-white hover:bg-[var(--bg-hover)]",
};

const SIZE = {
    sm: "px-3 text-xs",
    md: "px-4 text-sm",
    lg: "min-h-12 px-6 text-sm",
};

/* Button styling for elements that are not buttons, such as a Link. */
export function buttonClass(
    variant: NonNullable<ButtonProps["variant"]> = "secondary",
    size: NonNullable<ButtonProps["size"]> = "md",
    className?: string,
) {
    return cn(BASE, VARIANT[variant], variant !== "icon" && SIZE[size], className);
}

const Button = memo(forwardRef<HTMLButtonElement, ButtonProps>(
    (
        {
            className,
            variant = "secondary",
            size = "md",
            isLoading,
            children,
            disabled,
            ...props
        },
        ref
    ) => {
        return (
            <button
                ref={ref}
                className={buttonClass(variant, size, className)}
                disabled={disabled || isLoading}
                {...props}
            >
                {isLoading ? (
                    <>
                        <GradientSpinner size="sm" />
                        {children}
                    </>
                ) : (
                    children
                )}
            </button>
        );
    }
));

Button.displayName = "Button";

export { Button };
