//! Opt-in measurement only; no schema setup, writes, synthetic vectors or fallback.
use super::{queries, recall_at_20, uses_hnsw};
use crate::common;
use kima_db::vibe::{
    ELIGIBLE, MATCHING_SOUND,
    search::{SearchTrack, ef_for_limit, measurement_sql},
};
use kima_ident::stages::embedding::{self, NativeTextEmbedder};
use kima_ml::{onnx::OnnxModel, text_tokenizer};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    str::FromStr,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    checkpoint: String,
    audio_model_id: String,
    audio_sha256: String,
    pipeline_version: String,
    text_sha256: String,
    tokenizer_sha256: String,
    corpus_sha256: String,
    corpus_origin: String,
    query_origin: String,
    licensed_private_use_confirmed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authorization {
    host: String,
    port: u16,
    database: String,
    user: String,
    system_identifier: String,
    database_comment: String,
    isolated_copy_read_only: bool,
}

fn required(name: &str) -> Result<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} is required"))?)
}

fn read_input(path: &str) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err("input must be a regular file <= 1 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("input grew beyond limit".into());
    }
    Ok(bytes)
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Pure preconnection gate: never let a URL silently override the authorized identity.
fn authorized_options(
    url: &str,
    pg_database: &str,
    auth: &Authorization,
) -> Result<PgConnectOptions> {
    let opts = PgConnectOptions::from_str(url)?;
    if !auth.isolated_copy_read_only
        || auth.host != "127.0.0.1"
        || auth.port < 55474
        || auth.database == "kima"
        || auth.database.is_empty()
        || auth.user.is_empty()
        || auth.system_identifier.is_empty()
        || auth.database_comment.is_empty()
        || opts.get_host() != auth.host
        || opts.get_port() != auth.port
        || opts.get_database() != Some(auth.database.as_str())
        || pg_database != auth.database
        || opts.get_username() != auth.user
    {
        return Err(
            "SAFETY REFUSAL: database URL, PGDATABASE and isolated-copy authorization must agree"
                .into(),
        );
    }
    Ok(opts.options([("default_transaction_read_only", "on")]))
}

fn validate_manifest(m: &Manifest) -> Result<()> {
    if m.checkpoint != "laion-clap-music-onnx-v1"
        || !m.licensed_private_use_confirmed
        || m.audio_model_id != MATCHING_SOUND.model_id
        || m.audio_sha256 != hex::encode(MATCHING_SOUND.model_sha256)
        || m.pipeline_version != MATCHING_SOUND.pipeline_version
        || m.text_sha256 != hex::encode(OnnxModel::ClapText.expected_sha256())
        || m.tokenizer_sha256 != hex::encode(text_tokenizer::descriptor().expected_sha256)
        || m.corpus_origin.trim().is_empty()
        || m.query_origin.trim().is_empty()
        || m.corpus_sha256.len() != 64
        || !m.corpus_sha256.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(
            "checkpoint/manifest mismatch or missing provenance/licence attestation".into(),
        );
    }
    // Until M4 swaps its native checkpoint this deliberately fails, BEFORE connection.
    // A manifest cannot relabel hf_audio vectors as music_audio.
    if OnnxModel::ClapAudio.artifact_id().as_str() != MATCHING_SOUND.model_id
        || OnnxModel::ClapAudio.expected_sha256() != MATCHING_SOUND.model_sha256
        || embedding::PIPELINE_VERSION != MATCHING_SOUND.pipeline_version
    {
        return Err(
            "native checkpoint does not match MATCHING_SOUND: M4 music checkpoint swap required"
                .into(),
        );
    }
    Ok(())
}

struct Neighbors {
    exact: Vec<SearchTrack>,
    ann: Vec<SearchTrack>,
    plan: serde_json::Value,
    exact_ms: f64,
    ann_ms: f64,
}

async fn neighbors(
    conn: &mut sqlx::PgConnection,
    vector: &[f32],
    exact_sql: &str,
    ann_sql: &str,
) -> Result<Neighbors> {
    sqlx::raw_sql("SET LOCAL enable_seqscan=on; SET LOCAL enable_bitmapscan=on; SET LOCAL enable_sort=on; SET LOCAL jit=off").execute(&mut *conn).await?;
    let start = Instant::now();
    let exact = sqlx::query_as::<_, SearchTrack>(exact_sql)
        .persistent(false)
        .bind(vector)
        .bind(20i64)
        .fetch_all(&mut *conn)
        .await?;
    let exact_ms = start.elapsed().as_secs_f64() * 1000.0;
    sqlx::query("SELECT set_config('hnsw.ef_search',$1,true),set_config('hnsw.iterative_scan','relaxed_order',true),set_config('hnsw.max_scan_tuples','40000',true)").bind(ef_for_limit(20).to_string()).execute(&mut *conn).await?;
    sqlx::raw_sql(
        "SET LOCAL enable_seqscan=off; SET LOCAL enable_bitmapscan=off; SET LOCAL enable_sort=off",
    )
    .execute(&mut *conn)
    .await?;
    let plan = sqlx::query_scalar(&format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {ann_sql}"
    ))
    .persistent(false)
    .bind(vector)
    .bind(20i64)
    .fetch_one(&mut *conn)
    .await?;
    let start = Instant::now();
    let ann = sqlx::query_as::<_, SearchTrack>(ann_sql)
        .persistent(false)
        .bind(vector)
        .bind(20i64)
        .fetch_all(&mut *conn)
        .await?;
    let ann_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(Neighbors {
        exact,
        ann,
        plan,
        exact_ms,
        ann_ms,
    })
}

pub async fn measure() -> Result<()> {
    // Read all prerequisites before any connection/harness setup. No sqlx::test macro.
    let auth: Authorization =
        serde_json::from_slice(&read_input(&required("KIMA_RECALL_DB_AUTHORIZATION")?)?)?;
    let opts = authorized_options(&required("DATABASE_URL")?, &required("PGDATABASE")?, &auth)?;
    let manifest_bytes = read_input(&required("KIMA_RECALL_MANIFEST")?)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let query_bytes = read_input(&required("KIMA_RECALL_QUERIES")?)?;
    let texts = queries(&query_bytes)?;
    let model_dir = PathBuf::from(required("KIMA_CLAP_MODEL_DIR")?);
    if !model_dir.is_absolute() || !model_dir.is_dir() {
        return Err("absolute KIMA_CLAP_MODEL_DIR required".into());
    }
    let _ = required("ORT_DYLIB_PATH")?;
    let output = PathBuf::from(required("KIMA_RECALL_REPORT")?);
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    if !output.is_absolute()
        || output
            .parent()
            .ok_or("report has no parent")?
            .canonicalize()?
            .starts_with(repo)
    {
        return Err("private report must be an absolute path outside the repository".into());
    }
    // Private, append-progress JSONL; refuse overwrite. Store outside the repository.
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_lazy_with(opts);
    // Lazy pool: verify native assets before the first database connection.
    let ctx = common::ident_ctx_with_embedding(
        &pool,
        &model_dir,
        common::dead_bases(),
        kima_core::identify::IdentifySwitch::new(false, true),
        kima_ident::EmbeddingConfig {
            model_dir: Some(model_dir.clone()),
            disabled: false,
            pool_size: 1,
            ..Default::default()
        },
    );
    let status = embedding::initialize(&ctx).await?;
    if !status.is_available(kima_ml::Capability::ClapTextEmbedding)
        || !status.is_available(kima_ml::Capability::ClapAudioEmbedding)
    {
        return Err(format!("native matching text/audio assets unavailable: {status:?}").into());
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='60s'")
        .execute(&mut *tx)
        .await?;
    let (db, user, system, comment): (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT current_database()::text,current_user::text,(SELECT system_identifier::text FROM pg_control_system()),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()")
        .fetch_one(&mut *tx).await?;
    if db != auth.database
        || user != auth.user
        || system != auth.system_identifier
        || comment.as_deref() != Some(auth.database_comment.as_str())
    {
        return Err("SAFETY REFUSAL: server identity/comment mismatch".into());
    }
    let stale: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {} AND te.file_generation <> t.ml_generation",
        *ELIGIBLE
    ))
    .fetch_one(&mut *tx)
    .await?;
    if stale != 0 {
        return Err("eligible corpus contains stale file generations".into());
    }
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}", *ELIGIBLE))
        .fetch_one(&mut *tx)
        .await?;
    if count < 20 {
        return Err("need at least 20 eligible real embeddings".into());
    }
    let mut digest = Sha256::new();
    // Canonical UUID bytes + big-endian u64 byte length + pgvector vector_send bytes,
    // ordered by UUID. Binds evidence to ACTUAL stored binary vectors, not row counts.
    use futures::TryStreamExt;
    let corpus_sql = format!(
        "SELECT t.id,vector_send(te.embedding) FROM {} ORDER BY t.id",
        *ELIGIBLE
    );
    let mut stream = sqlx::query_as::<_, (uuid::Uuid, Vec<u8>)>(&corpus_sql).fetch(&mut *tx);
    while let Some((id, bytes)) = stream.try_next().await? {
        digest.update(id.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    drop(stream);
    let corpus_sha256 = hex::encode(digest.finalize());
    if corpus_sha256 != manifest.corpus_sha256 {
        return Err("actual corpus hash differs from predeclared real-copy manifest".into());
    }
    let (definition, options, valid, ready): (String, Vec<String>, bool, bool) = sqlx::query_as(
        "SELECT pg_get_indexdef(c.oid),c.reloptions,i.indisvalid,i.indisready FROM pg_class c JOIN pg_index i ON i.indexrelid=c.oid WHERE c.oid='track_embeddings_cosine_idx'::regclass")
        .fetch_one(&mut *tx).await?;
    if !valid
        || !ready
        || !definition.contains("USING hnsw")
        || !definition.contains("vector_cosine_ops")
        || !options.iter().any(|s| s == "m=16")
        || !options.iter().any(|s| s == "ef_construction=200")
    {
        return Err("expected valid construction200 HNSW index".into());
    }
    let (postgres, pgvector): (String, String) =
        sqlx::query_as("SELECT version(),extversion FROM pg_extension WHERE extname='vector'")
            .fetch_one(&mut *tx)
            .await?;
    writeln!(
        report,
        "{}",
        json!({"event":"start","schema":1,"gate":"Task14 real recall@20","checkpoint":manifest.checkpoint,"audio_model_id":manifest.audio_model_id,"audio_sha256":manifest.audio_sha256,"pipeline_version":manifest.pipeline_version,"text_sha256":manifest.text_sha256,"tokenizer_sha256":manifest.tokenizer_sha256,"corpus_origin":manifest.corpus_origin,"query_origin":manifest.query_origin,"manifest_sha256":sha(&manifest_bytes),"queries_sha256":sha(&query_bytes),"corpus_sha256":corpus_sha256,"eligible_count":count,"planning_count_only":15000,"database":db,"system_identifier":system,"postgres":postgres,"pgvector":pgvector,"index_definition":definition,"index_options":options,"ef_search":ef_for_limit(20),"iterative_scan":"relaxed_order","max_scan_tuples":40000,"planner_forcing":"test-only enable_seqscan=off, enable_bitmapscan=off, enable_sort=off, jit=off for ANN; EXPLAIN ANALYZE required","latency_scope":"instrumented DB queries, not production route benchmark","exact_fallback":false})
    )?;
    report.flush()?;
    // A proof of execution is mandatory; planner switches alone are not evidence.
    let (exact_sql, ann_sql) = measurement_sql();
    let mut recalls = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let start = Instant::now();
        let vector = embedding::vibe_embed_text(&ctx, text, &NativeTextEmbedder)
            .await
            .map_err(|e| format!("native text query {index}: {e:?}"))?;
        let embedding_ms = start.elapsed().as_secs_f64() * 1000.0;
        let Neighbors {
            exact,
            ann,
            plan,
            exact_ms,
            ann_ms,
        } = neighbors(&mut tx, vector.as_slice(), &exact_sql, &ann_sql).await?;
        let exact_ids: Vec<_> = exact.iter().map(|r| r.id).collect();
        let ann_ids: Vec<_> = ann.iter().map(|r| r.id).collect();
        let recall = recall_at_20(&exact_ids, &ann_ids)?;
        let index_used = uses_hnsw(&plan);
        writeln!(
            report,
            "{}",
            json!({"event":"query","index":index,"query_sha256":sha(text.as_bytes()),"vector":vector.as_slice(),"embedding_ms":embedding_ms,"exact_ms":exact_ms,"ann_ms":ann_ms,"exact_ids":exact_ids,"ann_ids":ann_ids,"exact_distances":exact.iter().map(|r| r.distance).collect::<Vec<_>>(),"ann_distances":ann.iter().map(|r| r.distance).collect::<Vec<_>>(),"recall_at_20":recall,"index_used":index_used,"explain_analyze":plan})
        )?;
        report.flush()?;
        if !index_used {
            return Err(format!(
                "query {index} did not execute HNSW; no recall pass can be claimed"
            )
            .into());
        }
        recalls.push(recall);
    }
    let mean = recalls.iter().sum::<f64>() / recalls.len() as f64;
    let minimum = recalls.iter().copied().fold(1.0, f64::min);
    // Binding recall gate is mean recall@20 >= .95; minimum is always disclosed.
    let passed = recalls.len() == 50 && mean >= 0.95;
    writeln!(
        report,
        "{}",
        json!({"event":"summary","queries":recalls.len(),"mean_recall_at_20":mean,"minimum_recall_at_20":minimum,"threshold":0.95,"passed":passed})
    )?;
    report.flush()?;
    tx.rollback().await?;
    pool.close().await;
    if !passed {
        return Err("real recall@20 gate failed; retain report without tuning queries".into());
    }
    Ok(())
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn synthetic_database_measurement_executes_shared_raw_ann_without_model(
    pool: kima_db::PgPool,
) {
    // Infrastructure test ONLY. No text inference and never a real acceptance result.
    let first = common::track(&pool, "synthetic-seed.flac", 30).await;
    let album: uuid::Uuid = sqlx::query_scalar("SELECT album_id FROM tracks WHERE id=$1")
        .bind(first)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint) SELECT md5('recall-synthetic-'||n)::uuid,$1,'synthetic','Track '||n,'flac','recall/'||n,1,now(),'\\x00' FROM generate_series(1,128) n").bind(album).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) SELECT md5('recall-synthetic-'||n)::uuid,0,$1,$2,$3,l2_normalize(ARRAY(SELECT (CASE k WHEN 0 THEN cos(n::float8/128) WHEN 1 THEN sin(n::float8/128) ELSE 0 END)::real FROM generate_series(0,511) k)::vector) FROM generate_series(1,128) n")
        .bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.as_slice()).bind(MATCHING_SOUND.pipeline_version).execute(&pool).await.unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .unwrap();
    let mut vector = vec![0.0; 512];
    vector[0] = 1.0;
    let (exact_sql, ann_sql) = measurement_sql();
    let result = neighbors(&mut tx, &vector, &exact_sql, &ann_sql)
        .await
        .unwrap();
    assert!(uses_hnsw(&result.plan), "{:#}", result.plan);
    assert_eq!(
        recall_at_20(
            &result.exact.iter().map(|r| r.id).collect::<Vec<_>>(),
            &result.ann.iter().map(|r| r.id).collect::<Vec<_>>()
        )
        .unwrap(),
        1.0
    );
    let bytes: Vec<u8> = sqlx::query_scalar(&format!(
        "SELECT vector_send(te.embedding) FROM {} LIMIT 1",
        *ELIGIBLE
    ))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(!bytes.is_empty());
    tx.rollback().await.unwrap();
}

#[test]
fn synthetic_manifest_rejects_native_checkpoint_mismatch_and_unlicensed_input() {
    let mut m = Manifest {
        checkpoint: "laion-clap-music-onnx-v1".into(),
        audio_model_id: MATCHING_SOUND.model_id.into(),
        audio_sha256: hex::encode(MATCHING_SOUND.model_sha256),
        pipeline_version: MATCHING_SOUND.pipeline_version.into(),
        text_sha256: hex::encode(OnnxModel::ClapText.expected_sha256()),
        tokenizer_sha256: hex::encode(text_tokenizer::descriptor().expected_sha256),
        corpus_sha256: "0".repeat(64),
        corpus_origin: "synthetic unit test, not a corpus".into(),
        query_origin: "synthetic unit test".into(),
        licensed_private_use_confirmed: true,
    };
    let native_matches = OnnxModel::ClapAudio.artifact_id().as_str() == MATCHING_SOUND.model_id
        && OnnxModel::ClapAudio.expected_sha256() == MATCHING_SOUND.model_sha256
        && embedding::PIPELINE_VERSION == MATCHING_SOUND.pipeline_version;
    assert_eq!(validate_manifest(&m).is_ok(), native_matches);
    m.licensed_private_use_confirmed = false;
    assert!(validate_manifest(&m).is_err());
    m.licensed_private_use_confirmed = true;
    m.audio_model_id = "hf_audio".into();
    assert!(validate_manifest(&m).is_err());
}

#[test]
fn synthetic_preconnection_guard_rejects_mismatched_or_live_targets() {
    let auth = Authorization {
        host: "127.0.0.1".into(),
        port: 55474,
        database: "copy".into(),
        user: "verify".into(),
        system_identifier: "synthetic".into(),
        database_comment: "authorized copy".into(),
        isolated_copy_read_only: true,
    };
    assert!(authorized_options("postgres://verify@127.0.0.1:55474/copy", "copy", &auth).is_ok());
    for (url, pg) in [
        ("postgres://verify@127.0.0.1:55474/copy", "other"),
        ("postgres://verify@127.0.0.1:5456/kima", "kima"),
        ("postgres://verify@127.0.0.1:55474/other", "other"),
        ("postgres://verify@remote:55474/copy", "copy"),
        ("postgres://other@127.0.0.1:55474/copy", "copy"),
    ] {
        assert!(authorized_options(url, pg, &auth).is_err());
    }
}
