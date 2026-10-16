import { useCallback, useEffect, useMemo, useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AnalysisWorkersConfig as WorkersConfig } from "@/lib/api";

/** One analyzer's worker count: the stored value, and a slider setter that saves once dragging stops. */
export function useWorkerSetting(
    queryKey: string,
    load: () => Promise<WorkersConfig>,
    save: (workers: number) => Promise<WorkersConfig>,
    fallbackRecommended: number,
) {
    const queryClient = useQueryClient();
    const key = useMemo(() => [queryKey], [queryKey]);

    const { data: config, isLoading } = useQuery({
        queryKey: key,
        queryFn: load,
        staleTime: 0,
    });

    const withWorkers = useCallback(
        (workers: number): WorkersConfig => {
            const cpuCores = config?.cpuCores || 4;
            return {
                workers,
                cpuCores,
                recommended: config?.recommended || fallbackRecommended,
                description: `Using ${workers} of ${cpuCores} available CPU cores`,
            };
        },
        [config, fallbackRecommended],
    );

    const mutation = useMutation({
        mutationFn: save,
        onSuccess: (saved) => queryClient.setQueryData(key, saved),
        // A refused save puts the slider back on the stored value, read again from the server.
        onError: () => queryClient.invalidateQueries({ queryKey: key }),
    });

    const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const setWorkers = useCallback(
        (workers: number) => {
            if (debounceRef.current) clearTimeout(debounceRef.current);
            void queryClient.cancelQueries({ queryKey: key });
            queryClient.setQueryData(key, withWorkers(workers));
            debounceRef.current = setTimeout(() => mutation.mutate(workers), 500);
        },
        [queryClient, key, withWorkers, mutation],
    );

    useEffect(() => {
        return () => {
            if (debounceRef.current) clearTimeout(debounceRef.current);
        };
    }, []);

    return { config, isLoading, setWorkers };
}
