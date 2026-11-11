import { SettingsScreen } from "@/features/settings/SettingsScreen";

// Static export: explicit page instead of upstream [page] runtime params.
export default function SettingsLibraryPage() {
    return <SettingsScreen slug="library" />;
}
