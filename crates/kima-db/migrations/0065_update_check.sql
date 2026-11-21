-- M5b Task 17: the update check (upstream `SystemSettings.updateChecksEnabled`, `lastUpdateCheck`,
-- `latestKnownVersion`, migration `20261013010000_update_checks`). On by default, as upstream.
ALTER TABLE system_settings
    ADD COLUMN update_checks_enabled boolean NOT NULL DEFAULT true,
    ADD COLUMN last_update_check timestamptz,
    ADD COLUMN latest_known_version text;
