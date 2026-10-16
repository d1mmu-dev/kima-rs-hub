'use client';

import { useEffect } from 'react';
import { Button } from "@/components/ui/Button";

export default function GlobalError({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  useEffect(() => {
    console.error('Global error:', error);
  }, [error]);

  return (
    <html>
      <body>
        <div className="flex h-screen items-center justify-center bg-black">
          <div className="text-center">
            <h2 className="text-2xl font-bold text-white mb-4">
              Application Error
            </h2>
            <p className="text-[var(--text-secondary)] mb-6">
              A critical error occurred. Please reload the page.
            </p>
            <Button
              variant="primary"
              onClick={reset}
            >
              Reload Application
            </Button>
          </div>
        </div>
      </body>
    </html>
  );
}
