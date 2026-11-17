-- D-184: owner-approved construction200, matching the pinned upstream HNSW build.
-- ALTER preserves the indexed expression, cosine opclass and any index predicate;
-- REINDEX is required because changing reloptions alone does not rebuild an existing graph.
-- Deliberately transactional: readers cannot observe a half-rebuilt index.
ALTER INDEX track_embeddings_cosine_idx SET (m = 16, ef_construction = 200);
REINDEX INDEX track_embeddings_cosine_idx;
