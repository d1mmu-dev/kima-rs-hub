-- Offline browser fixture; run only against the generated fixture library after its first scan.
-- No network, tag writes, runs, overrides, or existing match history are manufactured here.
\set ON_ERROR_STOP on
BEGIN;
DO $seed$
DECLARE
    album uuid;
    candidate uuid := '019a0000-0000-7000-8000-000000000023';
    pairs jsonb;
    snapshot jsonb := '{
      "id":"c0000023-0000-4000-8000-000000000001",
      "title":"The Woods (Remastered)","date":"2005-05-24","status":"Official","country":"US",
      "artist-credit":[{"name":"Sleater-Kinney","joinphrase":"","artist":{"id":"b0000023-0000-4000-8000-000000000001","name":"Sleater-Kinney","sort-name":"Sleater-Kinney"}}],
      "release-group":{"id":"d0000023-0000-4000-8000-000000000001","title":"The Woods","primary-type":"Album","first-release-date":"2005-05-24","genres":[]},
      "label-info":[{"catalog-number":"E2E-23","label":{"name":"Fixture Records"}}],"genres":[],
      "media":[{"position":1,"format":"CD","track-count":2,"tracks":[
        {"id":"70000023-0000-4000-8000-000000000001","position":1,"title":"Woods 1","length":6000,"recording":{"id":"80000023-0000-4000-8000-000000000001","title":"Woods 1","length":6000,"artist-credit":[{"name":"Sleater-Kinney","artist":{"id":"b0000023-0000-4000-8000-000000000001","name":"Sleater-Kinney"}}]}},
        {"id":"70000023-0000-4000-8000-000000000002","position":2,"title":"Woods 2","length":6000,"recording":{"id":"80000023-0000-4000-8000-000000000002","title":"Woods 2","length":6000,"artist-credit":[{"name":"Sleater-Kinney","artist":{"id":"b0000023-0000-4000-8000-000000000001","name":"Sleater-Kinney"}}]}}
      ]}]
    }';
BEGIN
    SELECT t.album_id INTO STRICT album FROM tracks t JOIN albums a ON a.id=t.album_id
    JOIN artists ar ON ar.id=a.artist_id
    WHERE t.path='Sleater-Kinney/2005 - The Woods/01 - Woods 1.mp3'
      AND t.missing_since IS NULL AND a.title='The Woods' AND ar.name='Sleater-Kinney';
    IF (SELECT count(*) FROM tracks WHERE album_id=album AND missing_since IS NULL) <> 2
       OR NOT EXISTS (SELECT 1 FROM tracks WHERE album_id=album AND missing_since IS NULL
           AND path='Sleater-Kinney/2005 - The Woods/02 - Woods 2.mp3') THEN
        RAISE EXCEPTION 'Expected exactly the two generated Woods tracks';
    END IF;
    IF EXISTS (SELECT 1 FROM autotag_runs WHERE album_id=album AND state<>'undone') THEN
        RAISE EXCEPTION 'Fixture album has a standing match run; undo it before reseeding';
    END IF;
    SELECT jsonb_agg(jsonb_build_object('trackId',t.id,'medium',1,'position',n.position,
      'releaseTrackMbid',n.mbid,'distance',0.0) ORDER BY n.position) INTO pairs
    FROM (VALUES ('Sleater-Kinney/2005 - The Woods/01 - Woods 1.mp3',1,'70000023-0000-4000-8000-000000000001'),
                 ('Sleater-Kinney/2005 - The Woods/02 - Woods 2.mp3',2,'70000023-0000-4000-8000-000000000002')) n(path,position,mbid)
    JOIN tracks t ON t.path=n.path AND t.album_id=album AND t.missing_since IS NULL;
    INSERT INTO autotag_candidates (id,album_id,release_mbid,release_group_mbid,origin,distance,
      recommendation,penalties,mapping,extra_tracks,missing_tracks,release,release_group)
    VALUES (candidate,album,(snapshot->>'id')::uuid,(snapshot->'release-group'->>'id')::uuid,
      'manual',0.02,'strong','{"album":0.02}',pairs,'[]','[]',snapshot,snapshot->'release-group')
    ON CONFLICT (album_id,release_mbid) DO UPDATE SET mapping=EXCLUDED.mapping,
      release=EXCLUDED.release,release_group=EXCLUDED.release_group,fetched_at=now()
    RETURNING id INTO candidate;
    INSERT INTO autotag_albums (album_id,state,recommendation,best_candidate,searched_at)
    VALUES (album,'proposed','strong',candidate,now())
    ON CONFLICT (album_id) DO UPDATE SET state='proposed',recommendation='strong',
      best_candidate=candidate,chosen_candidate=NULL,error=NULL,searched_at=now(),updated_at=now();
END $seed$;
COMMIT;
