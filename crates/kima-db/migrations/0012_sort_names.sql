-- Sort keys for Subsonic ordering: lower-cased, leading article dropped. The article list is
-- upstream's LEADING_ARTICLES (services/pgTextRules.ts).
ALTER TABLE artists ADD COLUMN sort_name text GENERATED ALWAYS AS (
    lower(regexp_replace(name, '^(the|a|an|le|la|les|los|las|die|der|das)\s+', '', 'i'))) STORED;
ALTER TABLE albums ADD COLUMN sort_name text GENERATED ALWAYS AS (
    lower(regexp_replace(title, '^(the|a|an|le|la|les|los|las|die|der|das)\s+', '', 'i'))) STORED;
CREATE INDEX artists_sort_name_idx ON artists (sort_name);
CREATE INDEX albums_sort_name_idx ON albums (sort_name);
