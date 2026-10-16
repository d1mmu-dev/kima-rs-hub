/**
 * Fisher-Yates shuffle algorithm - produces uniformly random permutations
 * Unlike .sort(() => Math.random() - 0.5) which is biased
 */
export function shuffleArray<T>(array: T[]): T[] {
  const result = [...array];
  for (let i = result.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [result[i], result[j]] = [result[j], result[i]];
  }
  return result;
}

/**
 * Whether the shuffle button can act, and its tooltip. Only music shuffles, and a
 * vibe queue (song path, match vibe, blend) always plays in order.
 */
export function shuffleState(
    hasMedia: boolean,
    playbackType: string | null,
    activeOperationType: string,
): { enabled: boolean; title: string } {
    if (!hasMedia || playbackType !== "track") return { enabled: false, title: "shuffle (music only)" };
    if (activeOperationType !== "idle") return { enabled: false, title: "a vibe queue plays in order" };
    return { enabled: true, title: "shuffle" };
}
