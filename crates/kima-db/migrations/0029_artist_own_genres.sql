-- A person's own genre list on an artist (`artists.user_genres`) as rows, one per genre key:
-- what Subsonic's genre list and genre filters read besides the track and album genres
-- (owner decision 2026-11-06, extends D-114 / D-166). Keys are `genres.key` (lower-case,
-- spaces collapsed); the name is the person's first spelling. Blank names and names starting
-- with `_` (upstream's internal markers) are not genres.
CREATE VIEW artist_own_genres AS
SELECT ar.id AS artist_id,
       lower(regexp_replace(btrim(u.name), '\s+', ' ', 'g')) AS key,
       (array_agg(regexp_replace(btrim(u.name), '\s+', ' ', 'g') ORDER BY u.ord))[1] AS name
FROM artists ar, unnest(ar.user_genres) WITH ORDINALITY AS u(name, ord)
WHERE btrim(u.name) <> '' AND btrim(u.name) NOT LIKE '\_%'
GROUP BY ar.id, 2;
