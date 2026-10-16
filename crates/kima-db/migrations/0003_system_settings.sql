-- The music folder the library scans; null until an admin sets it.
ALTER TABLE system_settings ADD COLUMN library_root text;
