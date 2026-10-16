"use client";

import { createContext, useContext } from "react";
import type { AudioController } from "./audio-controller";

export const AudioControllerContext = createContext<AudioController | null>(null);

export function useAudioController(): AudioController | null {
    return useContext(AudioControllerContext);
}
