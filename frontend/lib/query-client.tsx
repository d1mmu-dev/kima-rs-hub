"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReactQueryDevtools } from "@tanstack/react-query-devtools";

function makeQueryClient() {
    return new QueryClient({
        defaultOptions: {
            queries: {
                staleTime: 5 * 60 * 1000,
                gcTime: 10 * 60 * 1000,

                refetchOnWindowFocus: false, // a refetch on focus would interrupt playback
                refetchOnMount: true,
                refetchOnReconnect: true,

                retry: 1,
                retryDelay: (attemptIndex) => Math.min(1000 * 2 ** attemptIndex, 30000),

                throwOnError: false,
            },
            mutations: {
                retry: false,
            },
        },
    });
}

let browserQueryClient: QueryClient | undefined = undefined;

/** A new client per server render; one shared client in the browser. */
function getQueryClient() {
    if (typeof window === "undefined") {
        return makeQueryClient();
    } else {
        if (!browserQueryClient) browserQueryClient = makeQueryClient();
        return browserQueryClient;
    }
}

/**
 * QueryProvider component to wrap the app with React Query
 * Includes devtools in development mode
 */
export function QueryProvider({ children }: { children: React.ReactNode }) {
    const queryClient = getQueryClient();

    return (
        <QueryClientProvider client={queryClient}>
            {children}
            {process.env.NODE_ENV === "development" && (
                <ReactQueryDevtools
                    initialIsOpen={false}
                    buttonPosition="bottom-left"
                />
            )}
        </QueryClientProvider>
    );
}
