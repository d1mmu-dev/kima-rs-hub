import { Tab } from "../types";
import { cn } from "@/lib/cn";

interface LibraryTabsProps {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
}

// No per-tab colour: only the active segment carries the app accent.
const tabs = [
  { id: "artists" as Tab, label: "Artists" },
  { id: "albums" as Tab, label: "Albums" },
  { id: "tracks" as Tab, label: "Tracks" },
];

export function LibraryTabs({ activeTab, onTabChange }: LibraryTabsProps) {
  return (
    // Soft pills: the chosen one is amber text on a faint amber tint, the rest quiet.
    <div data-tv-section="library-tabs" className="inline-flex items-center gap-1">
      {tabs.map((tab, index) => {
        const isActive = activeTab === tab.id;

        return (
          <button
            key={tab.id}
            data-tv-card
            data-tv-card-index={index}
            tabIndex={0}
            aria-pressed={isActive}
            onClick={() => onTabChange(tab.id)}
            className={cn(
              // min-h meets the 44px touch target; the pill rounds fully.
              "min-h-[44px] rounded-full px-4 py-2 text-sm font-medium transition-colors duration-150",
              isActive
                ? "text-brand bg-brand/10"
                : "text-[var(--text-muted)] hover:text-white hover:bg-white/5",
            )}
          >
            {tab.label}
          </button>
        );
      })}
    </div>
  );
}
