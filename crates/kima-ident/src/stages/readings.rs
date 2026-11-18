//! Stored-vector zero-shot readings, before mood statistics. No audio decoding or detached writes.
use super::embedding::{NativeTextEmbedder, TextEmbedder, TextEmbeddingError, vibe_embed_text};
use crate::IdentCtx;
use kima_db::vibe::{MATCHING_SOUND, SoundIdentity, readings};
use kima_vibe::zero_shot::{PROMPTS, Readings};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// The native encoder's checkpoint association plus exact text/tokenizer pins.
/// Never infer music compatibility from vector width or an operator-provided directory name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptIdentity {
    pub sound: SoundIdentity,
    pub text_sha256: [u8; 32],
    pub tokenizer_sha256: [u8; 32],
}

/// Upstream c7527577 services/audio-analyzer-clap/Dockerfile:15,36 (not a hash of local bytes).
pub const MUSIC_TEXT_SHA256: [u8; 32] = [
    0x6d, 0xbb, 0xa0, 0x74, 0xb2, 0x7f, 0x89, 0x73, 0xd8, 0x4c, 0x5c, 0x7a, 0xe9, 0xac, 0xd0, 0xb9,
    0x13, 0x9a, 0x6c, 0xd9, 0xdd, 0x82, 0xbe, 0xa6, 0xe8, 0x79, 0x54, 0xdc, 0x3b, 0x83, 0x59, 0x56,
];

type PromptVectors = Arc<Vec<[f32; 512]>>;
#[derive(Default)]
pub struct PromptCache {
    entry: Mutex<Option<(PromptIdentity, PromptVectors)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingPass {
    Unavailable,
    Cancelled,
    Ran(u64),
}

/// Production always uses the verified native provider; a mismatched checkpoint skips only this step.
pub async fn run_pass(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<ReadingPass> {
    run_with(ctx, &NativeTextEmbedder, cancel).await
}

/// Injection boundary for synthetic tests, not a configuration override.
pub async fn run_with(
    ctx: &IdentCtx,
    provider: &dyn TextEmbedder,
    cancel: &CancellationToken,
) -> sqlx::Result<ReadingPass> {
    tokio::select! { biased;
        ()=cancel.cancelled()=>Ok(ReadingPass::Cancelled),
        result=run_inner(ctx,provider)=>result,
    }
}

async fn run_inner(ctx: &IdentCtx, provider: &dyn TextEmbedder) -> sqlx::Result<ReadingPass> {
    let Some(identity) = provider
        .readings_identity()
        .filter(|i| i.sound == MATCHING_SOUND)
    else {
        return Ok(ReadingPass::Unavailable);
    };
    // Single-flight initialization; publish a complete cache only. Cancellation/failure drops partial vectors.
    let mut entry = ctx.embedding_runtime.vibe_readings.entry.lock().await;
    if entry.as_ref().is_none_or(|(key, _)| *key != identity) {
        *entry = None;
        let mut vectors = Vec::with_capacity(24);
        for (_, prompts) in PROMPTS {
            for text in prompts {
                match vibe_embed_text(ctx, text, provider).await {
                    Ok(v) => vectors.push(v),
                    Err(TextEmbeddingError::Unavailable(_)) => return Ok(ReadingPass::Unavailable),
                    Err(e) => {
                        return Err(sqlx::Error::Protocol(format!("CLAP reading prompt: {e:?}")));
                    }
                }
            }
        }
        // Validate every cached vector before even the first track is published.
        Readings::compute(&vectors[0], &vectors).map_err(|e| sqlx::Error::Protocol(e.into()))?;
        if provider.readings_identity().as_ref() != Some(&identity) {
            return Ok(ReadingPass::Unavailable);
        }
        *entry = Some((identity.clone(), Arc::new(vectors)));
    }
    let prompts = entry.as_ref().expect("complete prompt cache").1.clone();
    drop(entry);
    let mut written = 0;
    loop {
        if provider.readings_identity().as_ref() != Some(&identity) {
            return Ok(ReadingPass::Unavailable);
        }
        let pending = readings::pending(&ctx.pool, 500).await?;
        if pending.is_empty() {
            break;
        }
        let mut rows = Vec::with_capacity(pending.len());
        for p in pending {
            let audio: [f32; 512] = p
                .embedding
                .try_into()
                .map_err(|_| sqlx::Error::Protocol("invalid reading vector dimensions".into()))?;
            let value =
                Readings::compute(&audio, &prompts).map_err(|e| sqlx::Error::Protocol(e.into()))?;
            rows.push((p.track_id, p.embedded_at, value));
        }
        if provider.readings_identity().as_ref() != Some(&identity) {
            return Ok(ReadingPass::Unavailable);
        }
        written += readings::upsert(&ctx.pool, &rows).await?;
    }
    Ok(ReadingPass::Ran(written))
}
