//! Tokenizer adapter for the music CLAP text model (`music_text.onnx`), matching the
//! upstream encoding contract at `kima-hub` commit
//! `c7527577d925b20f5adf410c7fd4e0c8ac250f69`
//! (`services/audio-analyzer-clap/music_clap_onnx.py`, `MusicClapText.tokens`):
//!
//! `transformers.RobertaTokenizer.from_pretrained('roberta-base')` called with
//! `padding='max_length', truncation=True, max_length=77` (`TEXT_MAX_LENGTH`, laion_clap
//! `hook.py:64-72`). The music text graph takes int64 `input_ids` and `attention_mask`, so every
//! prompt is padded with `<pad>` to 77 ids and the mask marks the real tokens
//! ([`ClapTextTokenizer::encode_padded`]). The general M4 text graph took unpadded ids only;
//! [`ClapTextTokenizer::encode`] keeps that truncated, unpadded encoding as the first step.
//!
//! This adapter uses the `tokenizers` crate (the same Rust implementation
//! backing HuggingFace's "fast" Python tokenizers) to load the pinned
//! `tokenizer.json` directly, instead of reimplementing RoBERTa BPE by hand.

use crate::{ArtifactDescriptor, ArtifactId, VerifiedBytes};

/// Basename of the pinned RoBERTa-base `tokenizer.json`
/// (revision `e2da8e2f811d1448a5b465c236feacd80ffbac7b`).
pub const TOKENIZER_BASENAME: &str = "tokenizer.json";

/// Upper bound for the pinned `tokenizer.json` (actual verified size is
/// 1,355,863 bytes).
pub const TOKENIZER_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Upstream's fixed prompt truncation length (`TEXT_MAX_LENGTH` in
/// `music_clap_onnx.py`), counting the `<s>`/`</s>` special tokens.
pub const CLAP_TEXT_MAX_TOKENS: usize = 77;

/// RoBERTa-base special token ids, pinned at the same tokenizer revision.
pub const CLAP_TEXT_BOS_ID: i64 = 0;
pub const CLAP_TEXT_EOS_ID: i64 = 2;
pub const CLAP_TEXT_PAD_ID: i64 = 1;

/// RoBERTa-base vocabulary size, pinned at the same tokenizer revision.
pub const CLAP_TEXT_VOCAB_SIZE: usize = 50_265;

/// Reject pathologically large raw prompts before tokenizing. This is
/// independent of (and much larger than) the model's own
/// [`CLAP_TEXT_MAX_TOKENS`] truncation, which only bounds the *encoded*
/// output length, not the cost of tokenizing an arbitrarily long string.
pub const MAX_INPUT_CHARS: usize = 20_000;

fn expected_sha256() -> [u8; 32] {
    const HEX: &str = "847bbeab6174d66a88898f729d52fa8d355fafe1bea101cf960dd404581df70e";
    let mut out = [0u8; 32];
    for (i, chunk) in out.iter_mut().enumerate() {
        *chunk = u8::from_str_radix(&HEX[i * 2..i * 2 + 2], 16).expect("valid pinned hex digest");
    }
    out
}

/// The pinned artifact id for the tokenizer, used by capability wiring.
pub fn artifact_id() -> ArtifactId {
    ArtifactId::from_static("tokenizer")
}

/// The pinned artifact descriptor for `tokenizer.json`.
pub fn descriptor() -> ArtifactDescriptor {
    ArtifactDescriptor {
        id: artifact_id(),
        basename: TOKENIZER_BASENAME,
        expected_sha256: expected_sha256(),
        max_bytes: TOKENIZER_MAX_BYTES,
    }
}

/// Errors loading or using the CLAP text tokenizer.
#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    /// The supplied bytes do not match this adapter's own hardcoded pin
    /// (id, size, or SHA-256), regardless of what the caller's
    /// [`VerifiedBytes`] claims.
    #[error("tokenizer artifact does not match the pinned tokenizer.json")]
    UntrustedArtifact,
    #[error("failed to load tokenizer.json: {0}")]
    Load(String),
    #[error("failed to configure tokenizer truncation: {0}")]
    Configure(String),
    #[error("input text exceeds the maximum accepted length ({len} > {max} chars)")]
    InputTooLarge { len: usize, max: usize },
    #[error("tokenizer encoding failed: {0}")]
    Encode(String),
}

/// Pure-Rust tokenizer adapter loading a hash-pinned `tokenizer.json` and
/// encoding prompts to int64 token ids matching upstream's exact
/// truncation/special-token behavior; [`ClapTextTokenizer::encode_padded`] adds the
/// music graph's padding and attention mask.
pub struct ClapTextTokenizer {
    inner: tokenizers::Tokenizer,
}

impl ClapTextTokenizer {
    /// Load from hash-verified bytes. Re-checks the bytes against this
    /// adapter's own hardcoded pin before trusting them: a caller-supplied
    /// [`VerifiedBytes`] id alone is not sufficient, since the descriptor
    /// used to produce it is caller-created (see
    /// `references/onnx-verification.md`).
    pub fn from_verified_bytes(artifact: &VerifiedBytes) -> Result<Self, TokenizerError> {
        use sha2::{Digest, Sha256};
        let expected = descriptor();
        if artifact.id() != expected.id
            || artifact.as_bytes().len() as u64 > expected.max_bytes
            || <[u8; 32]>::from(Sha256::digest(artifact.as_bytes())) != expected.expected_sha256
        {
            return Err(TokenizerError::UntrustedArtifact);
        }

        let mut inner = tokenizers::Tokenizer::from_bytes(artifact.as_bytes())
            .map_err(|e| TokenizerError::Load(e.to_string()))?;

        inner
            .with_truncation(Some(tokenizers::utils::truncation::TruncationParams {
                direction: tokenizers::utils::truncation::TruncationDirection::Right,
                max_length: CLAP_TEXT_MAX_TOKENS,
                strategy: tokenizers::utils::truncation::TruncationStrategy::LongestFirst,
                stride: 0,
            }))
            .map_err(|e| TokenizerError::Configure(e.to_string()))?;
        // Padding is applied explicitly by `encode_padded` (pad id and mask
        // checked by tests), not by the tokenizer.json's own padding config.
        inner.with_padding(None);

        Ok(Self { inner })
    }

    /// Encode one prompt to int64 token ids: `<s> ... </s>`, truncated to
    /// at most [`CLAP_TEXT_MAX_TOKENS`] total tokens, not padded.
    pub fn encode(&self, text: &str) -> Result<Vec<i64>, TokenizerError> {
        let len = text.chars().count();
        if len > MAX_INPUT_CHARS {
            return Err(TokenizerError::InputTooLarge {
                len,
                max: MAX_INPUT_CHARS,
            });
        }
        let encoding = self
            .inner
            .encode(text, true)
            .map_err(|e| TokenizerError::Encode(e.to_string()))?;
        Ok(encoding.get_ids().iter().map(|&id| i64::from(id)).collect())
    }
}

/// Pad one truncated encoding to [`CLAP_TEXT_MAX_TOKENS`] ids with [`CLAP_TEXT_PAD_ID`] and
/// return it with its attention mask (1 for each real token, 0 for padding), as
/// `padding='max_length'` does. Empty or over-length encodings are refused.
pub fn pad_to_max_length(ids: &[i64]) -> Result<(Vec<i64>, Vec<i64>), TokenizerError> {
    if ids.is_empty() || ids.len() > CLAP_TEXT_MAX_TOKENS {
        return Err(TokenizerError::Encode(format!(
            "encoding of {} tokens cannot be padded to {CLAP_TEXT_MAX_TOKENS}",
            ids.len()
        )));
    }
    let mut padded = ids.to_vec();
    padded.resize(CLAP_TEXT_MAX_TOKENS, CLAP_TEXT_PAD_ID);
    let mut mask = vec![1_i64; ids.len()];
    mask.resize(CLAP_TEXT_MAX_TOKENS, 0);
    Ok((padded, mask))
}

impl ClapTextTokenizer {
    /// The music text graph's input: [`Self::encode`], then [`pad_to_max_length`].
    pub fn encode_padded(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>), TokenizerError> {
        pad_to_max_length(&self.encode(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_pinned_basename_and_size_bound() {
        let desc = descriptor();
        assert_eq!(desc.basename, TOKENIZER_BASENAME);
        assert_eq!(desc.id, artifact_id());
        assert_ne!(desc.expected_sha256, [0u8; 32]);
        assert!(desc.max_bytes >= 1_355_863);
    }

    #[test]
    fn rejects_malformed_json_under_the_right_id() {
        // Bytes that are not even valid JSON must fail to load, not panic.
        let bytes = b"not json at all".to_vec().into_boxed_slice();
        // Build a VerifiedBytes carrying the right id but wrong bytes by
        // going through a temp dir + verify_artifact, matching the pinned
        // id so UntrustedArtifact is specifically about digest mismatch.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(TOKENIZER_BASENAME), &bytes).unwrap();
        let mut desc = descriptor();
        desc.expected_sha256 = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes).into()
        };
        let artifact = crate::verify_artifact(dir.path(), &desc).unwrap();
        // Hash matches *this* descriptor but not the adapter's hardcoded
        // pin, so loading must still be refused.
        assert!(matches!(
            ClapTextTokenizer::from_verified_bytes(&artifact),
            Err(TokenizerError::UntrustedArtifact)
        ));
    }
}
