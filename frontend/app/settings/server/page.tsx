import { SettingsScreen } from "@/features/settings/SettingsScreen";

// Static export: explicit page instead of upstream [page] runtime params.
export default function SettingsServerPage() {
    return <SettingsScreen slug="server" />;
}
