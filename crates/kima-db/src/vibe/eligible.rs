//! Which tracks the vibe engine sees, in one SQL fragment (spec 6.5).

use std::fmt::Write as _;
use std::sync::LazyLock;

/// The embedding checkpoint the feel probe, the mood heads and every stored vector of the engine are bound to:
/// model id, exact encoder bytes and the preprocessing pipeline (`track_embeddings.model_id/model_sha256/
/// pipeline_version`). Vectors under any other identity are invisible to the engine.
///
/// This is the one place to change when the embedding checkpoint changes. The values are the music checkpoint
/// `laion-clap-music-onnx-v1` (`music_audio.onnx`) that upstream's probe and heads were fitted on, as the M5 design
/// expects the M4 follow-up to name it; they are NOT what M4 writes today (`hf_audio`, `clap-audio-v1`), so a library
/// embedded by today's M4 has no eligible track until the checkpoint swap lands. The model id and pipeline strings
/// are unconfirmed until then.
pub const MATCHING_SOUND: SoundIdentity = SoundIdentity {
    model_id: "music_audio",
    model_sha256: [
        0xab, 0x1b, 0xf5, 0xeb, 0xa0, 0x67, 0xb2, 0x46, 0x19, 0x8c, 0x63, 0x0f, 0x15, 0x3f, 0x77,
        0x4e, 0x8f, 0x1c, 0x31, 0x69, 0xb3, 0x92, 0x4d, 0x4d, 0x6a, 0x4d, 0xc2, 0x47, 0xbd, 0x73,
        0xde, 0x97,
    ],
    pipeline_version: "clap-music-audio-v1",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundIdentity {
    pub model_id: &'static str,
    pub model_sha256: [u8; 32],
    pub pipeline_version: &'static str,
}

impl SoundIdentity {
    fn sha_hex(&self) -> String {
        self.model_sha256
            .iter()
            .fold(String::with_capacity(64), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
    }
}

/// `FROM`-clause tail plus `WHERE` of every vibe read: the tracks that have a vector of [`MATCHING_SOUND`]'s
/// identity with 512 dimensions and a non-zero norm, and whose file is present. Aliases: `te` (`track_embeddings`),
/// `t` (`tracks`). Append further conditions with ` AND ...`; start the statement with `SELECT ... FROM ` + this.
///
/// The identity is inlined as SQL literals (the model id and pipeline are checked to a safe alphabet by a test, the
/// hash is hex), so the fragment has no bind parameters and composes into any statement. M7 extends this fragment
/// when `albums.location` exists.
pub static ELIGIBLE: LazyLock<String> = LazyLock::new(|| {
    let id = MATCHING_SOUND;
    assert!(
        [id.model_id, id.pipeline_version]
            .iter()
            .all(|s| !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))),
        "MATCHING_SOUND strings are spliced into SQL and must stay in [A-Za-z0-9._-]"
    );
    format!(
        "track_embeddings te JOIN tracks t ON t.id = te.track_id \
         WHERE te.model_id = '{}' AND te.model_sha256 = decode('{}', 'hex') AND te.pipeline_version = '{}' \
         AND vector_dims(te.embedding) = 512 AND vector_norm(te.embedding) > 0 AND t.missing_since IS NULL",
        id.model_id,
        id.sha_hex(),
        id.pipeline_version
    )
});
