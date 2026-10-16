"use client";

import { ReactNode } from "react";
import { ErrorBoundary } from "@/components/providers/ErrorBoundary";
import { Button } from "@/components/ui/Button";

/** Fallback page for errors that reach the top of the app. */
export function GlobalErrorBoundary({ children }: { children: ReactNode }) {
    return (
        <ErrorBoundary
            label="GlobalErrorBoundary"
            renderFallback={(error) => (
                <div className="min-h-screen flex items-center justify-center bg-gray-950 text-white p-4">
                    <div className="max-w-md w-full text-center space-y-6">
                        <div className="space-y-2">
                            <h1 className="text-2xl font-bold">
                                Something went wrong
                            </h1>
                            <p className="text-[var(--text-secondary)]">
                                An unexpected error occurred. Please try
                                reloading the page.
                            </p>
                        </div>
                        {error && (
                            <div className="bg-gray-900 p-4 rounded-lg text-left">
                                <p className="text-sm tabular-nums text-danger-light break-words">
                                    {error.message}
                                </p>
                            </div>
                        )}
                        <Button
                            variant="primary"
                            size="lg"
                            onClick={() => window.location.reload()}
                            className="w-full"
                        >
                            Reload Page
                        </Button>
                    </div>
                </div>
            )}
        >
            {children}
        </ErrorBoundary>
    );
}
