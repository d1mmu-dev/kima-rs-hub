-- Empty-query song paging (Subsonic search3) orders by lower(title), id over present tracks.
CREATE INDEX tracks_title_sort_idx ON tracks (lower(title), id) WHERE missing_since IS NULL;
