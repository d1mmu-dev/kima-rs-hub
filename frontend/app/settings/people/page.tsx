import { SettingsScreen } from "@/features/settings/SettingsScreen";

// Static export: explicit page instead of upstream [page] runtime params.
export default function SettingsPeoplePage() {
    return <SettingsScreen slug="people" />;
}
