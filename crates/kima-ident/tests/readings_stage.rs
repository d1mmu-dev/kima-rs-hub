mod common;
use kima_db::{PgPool, vibe::MATCHING_SOUND};
use kima_ident::{
    IdentCtx,
    stages::{
        embedding::{NativeTextEmbedder, TextEmbedder, TextEmbeddingError, TextEmbeddingFuture},
        readings::{self, PromptIdentity, ReadingPass},
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
struct Provider {
    calls: Arc<AtomicUsize>,
    key: u8,
    unavailable: bool,
}
impl TextEmbedder for Provider {
    fn readings_identity(&self) -> Option<PromptIdentity> {
        Some(PromptIdentity {
            sound: MATCHING_SOUND,
            text_sha256: [self.key; 32],
            tokenizer_sha256: [3; 32],
        })
    }
    fn embed<'a>(&'a self, _: &'a IdentCtx, _: &'a str) -> TextEmbeddingFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.unavailable {
                return Err(TextEmbeddingError::Unavailable(
                    kima_ml::CapabilityEvaluationReport {
                        capabilities: Default::default(),
                    },
                ));
            }
            let mut v = [0.; 512];
            v[511] = 1.;
            Ok(v)
        })
    }
}
fn context(pool: &PgPool) -> (IdentCtx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (
        common::ident_ctx(pool, dir.path(), common::dead_bases()),
        dir,
    )
}
async fn embedded(pool: &PgPool) -> uuid::Uuid {
    let id = common::track(pool, &uuid::Uuid::new_v4().to_string(), 30).await;
    let mut v = vec![0f32; 512];
    v[511] = 1.;
    sqlx::query("INSERT INTO track_embeddings(track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) VALUES($1,0,$2,$3,$4,$5::real[]::vector)")
        .bind(id).bind(MATCHING_SOUND.model_id).bind(MATCHING_SOUND.model_sha256.to_vec()).bind(MATCHING_SOUND.pipeline_version).bind(v).execute(pool).await.unwrap();
    id
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn readings_wait_for_text_model(pool: PgPool) {
    let (ctx, _dir) = context(&pool);
    embedded(&pool).await;
    assert!(
        NativeTextEmbedder.readings_identity().is_none(),
        "current hf encoders cannot score music vectors"
    );
    assert_eq!(
        readings::run_pass(&ctx, &CancellationToken::new())
            .await
            .unwrap(),
        ReadingPass::Unavailable
    );
    let p = Provider {
        calls: Arc::new(AtomicUsize::new(0)),
        key: 1,
        unavailable: true,
    };
    assert_eq!(
        readings::run_with(&ctx, &p, &CancellationToken::new())
            .await
            .unwrap(),
        ReadingPass::Unavailable
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_clap_readings")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_stages WHERE status='failed'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prompt_cache_is_single_flight_and_invalidates_on_model_identity_change(pool: PgPool) {
    let (ctx, _dir) = context(&pool);
    embedded(&pool).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let p = Provider {
        calls: calls.clone(),
        key: 1,
        unavailable: false,
    };
    let cancel = CancellationToken::new();
    let (a, b) = tokio::join!(
        readings::run_with(&ctx, &p, &cancel),
        readings::run_with(&ctx, &p, &cancel)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 24);
    embedded(&pool).await;
    assert_eq!(
        readings::run_with(&ctx, &p, &cancel).await.unwrap(),
        ReadingPass::Ran(1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 24);
    let p = Provider {
        calls: calls.clone(),
        key: 2,
        unavailable: false,
    };
    embedded(&pool).await;
    readings::run_with(&ctx, &p, &cancel).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 48);
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancellation_at_publication_boundary_rolls_back_without_late_writes(pool: PgPool) {
    let (ctx, _dir) = context(&pool);
    let cancel = CancellationToken::new();
    let p = Provider {
        calls: Arc::new(AtomicUsize::new(0)),
        key: 1,
        unavailable: false,
    };
    readings::run_with(&ctx, &p, &cancel).await.unwrap();
    let id = embedded(&pool).await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *lock)
        .await
        .unwrap();
    let work = readings::run_with(&ctx, &p, &cancel);
    tokio::pin!(work);
    let waiting = async {
        loop {
            let yes:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FOR SHARE%')").fetch_one(&pool).await.unwrap();
            if yes {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::select! { _=&mut work=>panic!("publication must wait"), result=tokio::time::timeout(std::time::Duration::from_secs(5),waiting)=>{result.unwrap();} }
    cancel.cancel();
    assert_eq!(work.await.unwrap(), ReadingPass::Cancelled);
    lock.rollback().await.unwrap();
    // Wait for the dropped transaction's backend to finish processing rollback.
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND state='active' AND query LIKE '%FOR SHARE%')").fetch_one(&pool).await.unwrap();
            if !active {break;}
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_clap_readings")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancelled_readings_do_not_publish(pool: PgPool) {
    struct Blocking {
        entered: tokio::sync::Notify,
    }
    impl TextEmbedder for Blocking {
        fn readings_identity(&self) -> Option<PromptIdentity> {
            Some(PromptIdentity {
                sound: MATCHING_SOUND,
                text_sha256: [1; 32],
                tokenizer_sha256: [2; 32],
            })
        }
        fn embed<'a>(&'a self, _: &'a IdentCtx, _: &'a str) -> TextEmbeddingFuture<'a> {
            Box::pin(async move {
                self.entered.notify_one();
                std::future::pending().await
            })
        }
    }
    let (ctx, _dir) = context(&pool);
    embedded(&pool).await;
    let p = Blocking {
        entered: tokio::sync::Notify::new(),
    };
    let cancel = CancellationToken::new();
    let work = readings::run_with(&ctx, &p, &cancel);
    tokio::pin!(work);
    tokio::select! { _=&mut work=>panic!("must wait"), _=p.entered.notified()=>{} }
    cancel.cancel();
    assert_eq!(work.await.unwrap(), ReadingPass::Cancelled);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM track_clap_readings")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
