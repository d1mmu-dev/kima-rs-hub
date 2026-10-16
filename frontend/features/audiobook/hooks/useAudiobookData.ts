"use client";

import { useEffect } from "react";
import { useSearchParams } from "next/navigation";
import { useAudiobookQuery } from "@/hooks/useQueries";
import { api } from "@/lib/api";
import { subscribeQueryEvent } from "@/lib/query-events";

export function useAudiobookData() {
  const searchParams = useSearchParams();
  const audiobookId = searchParams.get("id") ?? "";

  const { data: audiobook, isLoading, refetch } = useAudiobookQuery(audiobookId);

  useEffect(() => {
    const unsubscribe = subscribeQueryEvent("audiobook-progress-updated", () => {
      refetch();
    });

    return unsubscribe;
  }, [refetch]);

  const heroImage = audiobook?.coverUrl
    ? api.getCoverArtUrl(audiobook.coverUrl, 1200)
    : null;
  const colorExtractionImage = audiobook?.coverUrl
    ? api.getCoverArtUrl(audiobook.coverUrl, 300, true)
    : null;

  return {
    audiobookId,
    audiobook,
    isLoading,
    refetch,
    heroImage,
    colorExtractionImage,
  };
}
