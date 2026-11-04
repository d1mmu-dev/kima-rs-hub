//! ONNX runtime session management and model loading.

mod resources;
#[cfg(test)]
mod tests;
use resources::ResourceReservation;

use ort::session::Session;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use crate::{ArtifactDescriptor, ArtifactId, Capability, ModelConsumer, VerifiedBytes};

/// Minimal valid synthetic ONNX model bytes implementing an Identity operator `y = Identity(x)`.
///
/// This minimal model is 78 bytes, conforms strictly to ONNX IR v7 with opset 13,
/// and requires no external weights, files, or downloads.
pub const MINIMAL_IDENTITY_ONNX: &[u8] = &[
    8, 7, 18, 4, 107, 105, 109, 97, 58, 64, 10, 16, 10, 1, 120, 18, 1, 121, 34, 8, 73, 100, 101,
    110, 116, 105, 116, 121, 18, 10, 116, 101, 115, 116, 95, 103, 114, 97, 112, 104, 90, 15, 10, 1,
    120, 18, 10, 10, 8, 8, 1, 18, 4, 10, 2, 8, 1, 98, 15, 10, 1, 121, 18, 10, 10, 8, 8, 1, 18, 4,
    10, 2, 8, 1, 66, 2, 16, 13,
];

const fn hex_byte(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("invalid hex"),
    }
}

const fn parse_hex_32(s: &str) -> [u8; 32] {
    let bytes = s.as_bytes();
    assert!(bytes.len() == 64, "expected 64 hex characters");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let hi = hex_byte(bytes[i * 2]);
        let lo = hex_byte(bytes[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum OnnxError {
    #[error("failed to initialize ONNX session builder: {0}")]
    SessionBuilder(String),

    #[error("failed to load ONNX model session for artifact {id}: {message}")]
    SessionInit { id: ArtifactId, message: String },

    #[error("session mutex lock poisoned")]
    LockPoisoned,

    #[error("ONNX inference failed: {0}")]
    Inference(String),

    #[error("missing required MusiCNN head: {0:?}")]
    MissingHead(MusiCnnHead),

    #[error("missing required MusiCNN core model")]
    MissingCore,
}

/// The 10 MusiCNN binary/multitask classification heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MusiCnnHead {
    Danceability,
    Deam,
    Emomusic,
    MoodAcoustic,
    MoodAggressive,
    MoodElectronic,
    MoodHappy,
    MoodParty,
    MoodRelaxed,
    MoodSad,
}

impl MusiCnnHead {
    pub const ALL: [MusiCnnHead; 10] = [
        MusiCnnHead::Danceability,
        MusiCnnHead::Deam,
        MusiCnnHead::Emomusic,
        MusiCnnHead::MoodAcoustic,
        MusiCnnHead::MoodAggressive,
        MusiCnnHead::MoodElectronic,
        MusiCnnHead::MoodHappy,
        MusiCnnHead::MoodParty,
        MusiCnnHead::MoodRelaxed,
        MusiCnnHead::MoodSad,
    ];

    pub fn all() -> &'static [MusiCnnHead] {
        &Self::ALL
    }

    pub const fn basename(&self) -> &'static str {
        match self {
            Self::Danceability => "danceability-msd-musicnn-1.onnx",
            Self::Deam => "deam-msd-musicnn-2.onnx",
            Self::Emomusic => "emomusic-msd-musicnn-2.onnx",
            Self::MoodAcoustic => "mood_acoustic-msd-musicnn-1.onnx",
            Self::MoodAggressive => "mood_aggressive-msd-musicnn-1.onnx",
            Self::MoodElectronic => "mood_electronic-msd-musicnn-1.onnx",
            Self::MoodHappy => "mood_happy-msd-musicnn-1.onnx",
            Self::MoodParty => "mood_party-msd-musicnn-1.onnx",
            Self::MoodRelaxed => "mood_relaxed-msd-musicnn-1.onnx",
            Self::MoodSad => "mood_sad-msd-musicnn-1.onnx",
        }
    }

    pub fn artifact_id(&self) -> ArtifactId {
        ArtifactId::from_static(self.basename())
    }

    pub const fn expected_sha256(&self) -> [u8; 32] {
        match self {
            Self::Danceability => {
                parse_hex_32("81610cb8fd344bff604e2b917c8eaaad997691a8d3cf7cbd0f6b1c8cf18173bd")
            }
            Self::Deam => {
                parse_hex_32("b512abd778485b389122887c02120d0e116c1adbfb6aed93d57df50dd067392e")
            }
            Self::Emomusic => {
                parse_hex_32("d76a443e89d8341c4b087e61b5e89026310d4daf54d244adae71c505d8d8008b")
            }
            Self::MoodAcoustic => {
                parse_hex_32("b4ccea97097e7822da38a93b373091295dc2c0a723c39c71bbbbc01a5b098f81")
            }
            Self::MoodAggressive => {
                parse_hex_32("cf9c68599970d8c40ddde4848c92c797e73d462d9644b50d62b5e429f107c972")
            }
            Self::MoodElectronic => {
                parse_hex_32("4a0ed7ead56a0e378bccaee5f6af0d6ab7225a04876d151aa40afa9782ca39ba")
            }
            Self::MoodHappy => {
                parse_hex_32("00d00674efeb79192ba0049ff110db14e6fe10a2750f0ab8aecff418c9bb3c27")
            }
            Self::MoodParty => {
                parse_hex_32("3bb436051a64c6e50a20dccfa31c85e6dea9b5cb15bbfc5c308104fbbd9a123f")
            }
            Self::MoodRelaxed => {
                parse_hex_32("3c407f3ebc16d30ebec418041f276b7f47e8dd09c2cc562575fbc092b7b01847")
            }
            Self::MoodSad => {
                parse_hex_32("a9a8ec64c741fea64cc99fc5e7a088d7d25373b89cf9a23850f8dec847963aa3")
            }
        }
    }

    pub fn descriptor(&self) -> ArtifactDescriptor {
        ArtifactDescriptor {
            id: self.artifact_id(),
            basename: self.basename(),
            expected_sha256: self.expected_sha256(),
            max_bytes: 10 * 1024 * 1024, // 10 MB per head model
        }
    }

    /// The exact pinned ONNX output tensor name for this specific head, derived
    /// from a one-time audit of the real exported graphs (see
    /// `references/onnx-verification.md`): Deam and Emomusic export a bare
    /// `Identity` regression output, every other head exports a `Softmax`
    /// classification output. Re-introspect the actual graph before copying
    /// this pattern to a newly added head.
    pub const fn expected_output_name(&self) -> &'static str {
        match self {
            Self::Deam | Self::Emomusic => "model/Identity:0",
            _ => "model/Softmax:0",
        }
    }
}

/// Supported ONNX runtime models for MusiCNN and CLAP.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OnnxModel {
    MusiCnnCore,
    MusiCnnHead(MusiCnnHead),
    ClapAudio,
    ClapText,
}

impl OnnxModel {
    pub const CORE_BASENAME: &'static str = "msd-musicnn-1.onnx";
    pub const CLAP_AUDIO_BASENAME: &'static str = "hf_audio.onnx";
    pub const CLAP_TEXT_BASENAME: &'static str = "text_model.onnx";

    pub fn all_models() -> Vec<OnnxModel> {
        let mut list = vec![OnnxModel::MusiCnnCore];
        for head in MusiCnnHead::all() {
            list.push(OnnxModel::MusiCnnHead(*head));
        }
        list.push(OnnxModel::ClapAudio);
        list.push(OnnxModel::ClapText);
        list
    }

    pub fn all_musicnn_models() -> Vec<OnnxModel> {
        let mut list = vec![OnnxModel::MusiCnnCore];
        for head in MusiCnnHead::all() {
            list.push(OnnxModel::MusiCnnHead(*head));
        }
        list
    }

    pub fn basename(&self) -> &'static str {
        match self {
            Self::MusiCnnCore => Self::CORE_BASENAME,
            Self::MusiCnnHead(head) => head.basename(),
            Self::ClapAudio => Self::CLAP_AUDIO_BASENAME,
            Self::ClapText => Self::CLAP_TEXT_BASENAME,
        }
    }

    pub fn artifact_id(&self) -> ArtifactId {
        match self {
            Self::MusiCnnCore => ArtifactId::from_static("msd-musicnn-1"),
            Self::MusiCnnHead(head) => head.artifact_id(),
            Self::ClapAudio => ArtifactId::from_static("hf_audio"),
            Self::ClapText => ArtifactId::from_static("text_model"),
        }
    }

    pub fn expected_sha256(&self) -> [u8; 32] {
        match self {
            Self::MusiCnnCore => {
                parse_hex_32("d30492af1609b75f47881a326cb402dfea3068f44c9482c5583e93768c1be885")
            }
            Self::MusiCnnHead(head) => head.expected_sha256(),
            Self::ClapAudio => {
                parse_hex_32("37df17afb9881569c4f50e5b691167aa54586418059701a74a54934514969972")
            }
            Self::ClapText => {
                parse_hex_32("6df7c51f2d78e236a03f2b816e613d538c815639d13644fe7c2124f439da9648")
            }
        }
    }

    pub fn max_bytes(&self) -> u64 {
        match self {
            Self::MusiCnnCore => 30 * 1024 * 1024,    // 30 MB
            Self::MusiCnnHead(_) => 10 * 1024 * 1024, // 10 MB
            Self::ClapAudio => 300 * 1024 * 1024,     // 300 MB
            Self::ClapText => 500 * 1024 * 1024,      // 500 MB
        }
    }

    pub fn descriptor(&self) -> ArtifactDescriptor {
        ArtifactDescriptor {
            id: self.artifact_id(),
            basename: self.basename(),
            expected_sha256: self.expected_sha256(),
            max_bytes: self.max_bytes(),
        }
    }

    pub fn requirements_by_capability() -> BTreeMap<Capability, Vec<ArtifactDescriptor>> {
        let mut map = BTreeMap::new();

        let musicnn_descs: Vec<ArtifactDescriptor> = Self::all_musicnn_models()
            .into_iter()
            .map(|m| m.descriptor())
            .collect();
        map.insert(Capability::MusicnnAnalysis, musicnn_descs);

        map.insert(
            Capability::ClapAudioEmbedding,
            vec![Self::ClapAudio.descriptor()],
        );
        map.insert(
            Capability::ClapTextEmbedding,
            vec![
                crate::text_tokenizer::descriptor(),
                Self::ClapText.descriptor(),
            ],
        );
        map.insert(Capability::Dsp, vec![]);

        map
    }
}

/// A shared, thread-safe ONNX model session.
#[derive(Clone, Debug)]
pub struct SharedSession {
    id: ArtifactId,
    session: Arc<Mutex<Session>>,
    _reservation: Arc<ResourceReservation>,
}

impl SharedSession {
    fn new(id: ArtifactId, session: Session, reservation: ResourceReservation) -> Self {
        Self {
            id,
            session: Arc::new(Mutex::new(session)),
            _reservation: Arc::new(reservation),
        }
    }

    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// Execute an inference closure with exclusive access to the underlying session.
    pub fn run_with<F, R>(&self, f: F) -> Result<R, OnnxError>
    where
        F: FnOnce(&mut Session) -> Result<R, OnnxError>,
    {
        let mut guard = self.session.lock().map_err(|_| OnnxError::LockPoisoned)?;
        f(&mut guard)
    }

    /// Run inference with 1D float array input and output (common pattern for identity/tests).
    pub fn run_1d(&self, input: &[f32]) -> Result<Vec<f32>, OnnxError> {
        self.run_with(|sess| {
            let shape = [input.len()];
            let tensor = ort::value::Tensor::from_array((shape, input.to_vec().into_boxed_slice()))
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let outputs = sess
                .run(ort::inputs![tensor])
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            if outputs.len() == 0 {
                return Err(OnnxError::Inference("empty model outputs".to_string()));
            }
            let out_tensor = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            Ok(out_tensor.1.to_vec())
        })
    }
}

/// Typed CLAP audio inference over a resource-accounted pool of verified sessions.
/// The caller supplies contiguous [batch, 1, time, 64] log-mel values.
pub struct ClapAudioSession {
    pool: BoundedSessionPool,
}

impl ClapAudioSession {
    pub fn from_verified_bytes(
        artifact: &VerifiedBytes,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        use sha2::{Digest, Sha256};
        let expected = OnnxModel::ClapAudio.descriptor();
        if artifact.id() != expected.id
            || artifact.as_bytes().len() as u64 > expected.max_bytes
            || <[u8; 32]>::from(Sha256::digest(artifact.as_bytes())) != expected.expected_sha256
        {
            return Err(OnnxError::SessionInit {
                id: artifact.id(),
                message: "CLAP audio artifact does not match the pinned model".into(),
            });
        }
        let model = Self::from_trusted_bytes(artifact, pool_size, threads)?;
        model.pool.with_session(|session| {
            use ort::{tensor::TensorElementType, value::ValueType};
            let matches_tensor = |value: &ValueType, dims: &[i64]| {
                matches!(value, ValueType::Tensor { ty: TensorElementType::Float32, shape, .. } if shape.as_ref() == dims)
            };
            if session.inputs.len() != 1
                || session.inputs[0].name != "input"
                || !matches_tensor(&session.inputs[0].input_type, &[-1, 1, -1, 64])
                || session.outputs.len() != 2
                || session.outputs[0].name != "output"
                || !matches_tensor(&session.outputs[0].output_type, &[-1, 512])
                || session.outputs[1].name != "onnx::Shape_9884"
                || !matches_tensor(&session.outputs[1].output_type, &[-1, -1, -1, -1])
            {
                return Err(OnnxError::Inference("invalid pinned CLAP graph signature".into()));
            }
            Ok(())
        })?;
        Ok(model)
    }

    fn from_trusted_bytes(
        artifact: &VerifiedBytes,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        Ok(Self {
            pool: BoundedSessionPool::from_verified_bytes(artifact, pool_size, threads)?,
        })
    }

    /// Accept 1–5 windows and 1–1001 frames, with 64 bands per frame.
    /// Returns raw finite model vectors; normalization is a separate step.
    pub fn infer(
        &self,
        input: &[f32],
        batch: usize,
        time: usize,
    ) -> Result<Vec<[f32; 512]>, OnnxError> {
        if !(1..=crate::clap::MAX_CLAP_WINDOWS).contains(&batch)
            || !(1..=1001).contains(&time)
            || batch.checked_mul(time).and_then(|n| n.checked_mul(64)) != Some(input.len())
            || input.iter().any(|x| !x.is_finite())
        {
            return Err(OnnxError::Inference(
                "invalid CLAP input shape or non-finite value".into(),
            ));
        }
        self.pool.with_session(|session| {
            let tensor = ort::value::Tensor::from_array((
                [batch, 1, time, 64],
                input.to_vec().into_boxed_slice(),
            ))
            .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output_buffer = ort::value::Tensor::from_array((
                [batch, 512],
                vec![0.0_f32; batch * 512].into_boxed_slice(),
            ))
            .map_err(|e| OnnxError::Inference(e.to_string()))?;
            // Selecting a fetch does not guarantee ORT prunes unrelated graph
            // nodes; the pinned graph and bounded inputs remain essential.
            let options = ort::session::run_options::RunOptions::new()
                .map_err(|e| OnnxError::Inference(e.to_string()))?
                .with_outputs(
                    ort::session::run_options::OutputSelector::no_default()
                        .with("output")
                        .preallocate("output", output_buffer),
                );
            let outputs = session
                .run_with_options(ort::inputs!["input" => tensor], &options)
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output = outputs
                .get("output")
                .ok_or_else(|| OnnxError::Inference("missing CLAP output".into()))?;
            let (shape, values) = output
                .try_extract_tensor::<f32>()
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            if shape.as_ref() != [batch as i64, 512] || values.iter().any(|x| !x.is_finite()) {
                return Err(OnnxError::Inference(
                    "invalid CLAP output shape or non-finite value".into(),
                ));
            }
            values
                .chunks_exact(512)
                .map(|v| {
                    v.try_into()
                        .map_err(|_| OnnxError::Inference("invalid CLAP output length".into()))
                })
                .collect()
        })
    }
}

/// Typed MusiCNN core backbone inference over a bounded pool of verified sessions.
/// Accepts contiguous `[batch, 187, 96]` f32 log-mel patches and returns bounded,
/// finite `[batch, 200]` embeddings, checking the exact trusted graph signature.
pub struct MusicnnCoreSession {
    pool: BoundedSessionPool,
}

/// Maximum MusiCNN patches accepted per inference call, matching the bounded
/// resource policy used elsewhere for ONNX batch sizes.
pub const MAX_MUSICNN_BATCH: usize = 64;

impl MusicnnCoreSession {
    pub fn from_verified_bytes(
        artifact: &VerifiedBytes,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        use sha2::{Digest, Sha256};
        let expected = OnnxModel::MusiCnnCore.descriptor();
        if artifact.id() != expected.id
            || artifact.as_bytes().len() as u64 > expected.max_bytes
            || <[u8; 32]>::from(Sha256::digest(artifact.as_bytes())) != expected.expected_sha256
        {
            return Err(OnnxError::SessionInit {
                id: artifact.id(),
                message: "MusiCNN core artifact does not match the pinned model".into(),
            });
        }
        let model = Self::from_trusted_bytes(artifact, pool_size, threads)?;
        model.pool.with_session(|session| {
            use ort::{tensor::TensorElementType, value::ValueType};
            let matches_tensor = |value: &ValueType, dims: &[i64]| {
                matches!(value, ValueType::Tensor { ty: TensorElementType::Float32, shape, .. } if shape.as_ref() == dims)
            };
            if session.inputs.len() != 1
                || session.inputs[0].name != "model/Placeholder:0"
                || !matches_tensor(&session.inputs[0].input_type, &[-1, 187, 96])
                || session.outputs.len() != 1
                || session.outputs[0].name != "model/dense/BiasAdd:0"
                || !matches_tensor(&session.outputs[0].output_type, &[-1, 200])
            {
                return Err(OnnxError::Inference(
                    "invalid pinned MusiCNN core graph signature".into(),
                ));
            }
            Ok(())
        })?;
        Ok(model)
    }

    fn from_trusted_bytes(
        artifact: &VerifiedBytes,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        Ok(Self {
            pool: BoundedSessionPool::from_verified_bytes(artifact, pool_size, threads)?,
        })
    }

    /// Accept `1..=MAX_MUSICNN_BATCH` contiguous `[187, 96]` log-mel patches.
    /// Returns bounded, finite `[batch, 200]` embeddings.
    pub fn infer(&self, input: &[f32], batch: usize) -> Result<Vec<[f32; 200]>, OnnxError> {
        if !(1..=MAX_MUSICNN_BATCH).contains(&batch)
            || batch.checked_mul(187).and_then(|n| n.checked_mul(96)) != Some(input.len())
            || input.iter().any(|x| !x.is_finite())
        {
            return Err(OnnxError::Inference(
                "invalid MusiCNN core input shape or non-finite value".into(),
            ));
        }
        self.pool.with_session(|session| {
            let tensor = ort::value::Tensor::from_array((
                [batch, 187, 96],
                input.to_vec().into_boxed_slice(),
            ))
            .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output_buffer = ort::value::Tensor::from_array((
                [batch, 200],
                vec![0.0_f32; batch * 200].into_boxed_slice(),
            ))
            .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let options = ort::session::run_options::RunOptions::new()
                .map_err(|e| OnnxError::Inference(e.to_string()))?
                .with_outputs(
                    ort::session::run_options::OutputSelector::no_default()
                        .with("model/dense/BiasAdd:0")
                        .preallocate("model/dense/BiasAdd:0", output_buffer),
                );
            let outputs = session
                .run_with_options(ort::inputs!["model/Placeholder:0" => tensor], &options)
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output = outputs
                .get("model/dense/BiasAdd:0")
                .ok_or_else(|| OnnxError::Inference("missing MusiCNN core output".into()))?;
            let (shape, values) = output
                .try_extract_tensor::<f32>()
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            if shape.as_ref() != [batch as i64, 200] || values.iter().any(|x| !x.is_finite()) {
                return Err(OnnxError::Inference(
                    "invalid MusiCNN core output shape or non-finite value".into(),
                ));
            }
            values
                .chunks_exact(200)
                .map(|v| {
                    v.try_into().map_err(|_| {
                        OnnxError::Inference("invalid MusiCNN core output length".into())
                    })
                })
                .collect()
        })
    }
}

/// Typed MusiCNN classification/regression head inference over a bounded pool
/// of verified sessions. Accepts `[batch, 200]` f32 core embeddings and returns
/// bounded, finite `[batch, 2]` outputs, checking the exact trusted graph for
/// the specific head.
pub struct MusicnnHeadSession {
    head: MusiCnnHead,
    pool: BoundedSessionPool,
}

impl MusicnnHeadSession {
    pub fn from_verified_bytes(
        artifact: &VerifiedBytes,
        head: MusiCnnHead,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        use sha2::{Digest, Sha256};
        let expected = head.descriptor();
        if artifact.id() != expected.id
            || artifact.as_bytes().len() as u64 > expected.max_bytes
            || <[u8; 32]>::from(Sha256::digest(artifact.as_bytes())) != expected.expected_sha256
        {
            return Err(OnnxError::SessionInit {
                id: artifact.id(),
                message: "MusiCNN head artifact does not match the pinned model".into(),
            });
        }
        Self::from_trusted_bytes(artifact, head, pool_size, threads)
    }

    fn from_trusted_bytes(
        artifact: &VerifiedBytes,
        head: MusiCnnHead,
        pool_size: usize,
        threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        let pool = BoundedSessionPool::from_verified_bytes(artifact, pool_size, threads)?;
        let model = Self { head, pool };
        model.pool.with_session(|session| {
            use ort::{tensor::TensorElementType, value::ValueType};
            let matches_tensor = |value: &ValueType, dims: &[i64]| {
                matches!(value, ValueType::Tensor { ty: TensorElementType::Float32, shape, .. } if shape.as_ref() == dims)
            };
            if session.inputs.len() != 1
                || session.inputs[0].name != "model/Placeholder:0"
                || !matches_tensor(&session.inputs[0].input_type, &[-1, 200])
                || session.outputs.len() != 1
                || !matches_tensor(&session.outputs[0].output_type, &[-1, 2])
            {
                return Err(OnnxError::Inference(
                    "invalid pinned MusiCNN head graph signature".into(),
                ));
            }
            let output_name = session.outputs[0].name.clone();
            if output_name != head.expected_output_name() {
                return Err(OnnxError::Inference(
                    "MusiCNN head output name does not match the pinned name for this head"
                        .into(),
                ));
            }
            Ok(())
        })?;
        Ok(model)
    }

    /// Accept `1..=MAX_MUSICNN_BATCH` `[200]` core embeddings. Returns bounded,
    /// finite `[batch, 2]` head outputs.
    pub fn infer(&self, embeddings: &[[f32; 200]]) -> Result<Vec<[f32; 2]>, OnnxError> {
        let batch = embeddings.len();
        if !(1..=MAX_MUSICNN_BATCH).contains(&batch) {
            return Err(OnnxError::Inference(
                "invalid MusiCNN head batch size".into(),
            ));
        }
        let flat: Vec<f32> = embeddings.iter().flatten().copied().collect();
        if flat.iter().any(|x| !x.is_finite()) {
            return Err(OnnxError::Inference(
                "non-finite MusiCNN head input value".into(),
            ));
        }
        let output_name = self.head.expected_output_name();
        self.pool.with_session(|session| {
            let tensor = ort::value::Tensor::from_array(([batch, 200], flat.into_boxed_slice()))
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output_buffer = ort::value::Tensor::from_array((
                [batch, 2],
                vec![0.0_f32; batch * 2].into_boxed_slice(),
            ))
            .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let options = ort::session::run_options::RunOptions::new()
                .map_err(|e| OnnxError::Inference(e.to_string()))?
                .with_outputs(
                    ort::session::run_options::OutputSelector::no_default()
                        .with(output_name)
                        .preallocate(output_name, output_buffer),
                );
            let outputs = session
                .run_with_options(ort::inputs!["model/Placeholder:0" => tensor], &options)
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            let output = outputs
                .get(output_name)
                .ok_or_else(|| OnnxError::Inference("missing MusiCNN head output".into()))?;
            let (shape, values) = output
                .try_extract_tensor::<f32>()
                .map_err(|e| OnnxError::Inference(e.to_string()))?;
            if shape.as_ref() != [batch as i64, 2] || values.iter().any(|x| !x.is_finite()) {
                return Err(OnnxError::Inference(
                    "invalid MusiCNN head output shape or non-finite value".into(),
                ));
            }
            values
                .chunks_exact(2)
                .map(|v| {
                    v.try_into().map_err(|_| {
                        OnnxError::Inference("invalid MusiCNN head output length".into())
                    })
                })
                .collect()
        })
    }
}

/// Shared container holding all 11 verified MusiCNN sessions (core + 10 heads).
#[derive(Clone, Debug)]
pub struct MusiCnnSessions {
    core: SharedSession,
    heads: BTreeMap<MusiCnnHead, SharedSession>,
}

impl MusiCnnSessions {
    pub fn new(
        core: SharedSession,
        heads: BTreeMap<MusiCnnHead, SharedSession>,
    ) -> Result<Self, OnnxError> {
        let core_id = core.id().as_str();
        if core_id != "msd-musicnn-1" && core_id != "msd-musicnn-1.onnx" {
            return Err(OnnxError::MissingCore);
        }
        for head in MusiCnnHead::all() {
            if !heads.contains_key(head) {
                return Err(OnnxError::MissingHead(*head));
            }
        }
        Ok(Self { core, heads })
    }

    pub fn core(&self) -> &SharedSession {
        &self.core
    }

    pub fn head(&self, head: MusiCnnHead) -> &SharedSession {
        &self.heads[&head]
    }

    pub fn heads(&self) -> &BTreeMap<MusiCnnHead, SharedSession> {
        &self.heads
    }
}

/// Registry holding loaded ONNX model sessions.
#[derive(Clone, Debug, Default)]
pub struct ModelSessionRegistry {
    sessions: BTreeMap<ArtifactId, SharedSession>,
}

impl ModelSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, session: SharedSession) {
        self.sessions.insert(session.id().clone(), session);
    }

    pub fn get(&self, id: &ArtifactId) -> Option<&SharedSession> {
        self.sessions.get(id)
    }

    pub fn sessions(&self) -> &BTreeMap<ArtifactId, SharedSession> {
        &self.sessions
    }

    pub fn build_musicnn(&self) -> Result<MusiCnnSessions, OnnxError> {
        let core = self
            .get(&ArtifactId::from_static("msd-musicnn-1"))
            .or_else(|| self.get(&ArtifactId::from_static("msd-musicnn-1.onnx")))
            .ok_or(OnnxError::MissingCore)?
            .clone();

        let mut heads = BTreeMap::new();
        for head in MusiCnnHead::all() {
            let session = self
                .get(&head.artifact_id())
                .or_else(|| self.get(&ArtifactId::from_static(head.basename())))
                .ok_or(OnnxError::MissingHead(*head))?
                .clone();
            heads.insert(*head, session);
        }

        MusiCnnSessions::new(core, heads)
    }

    pub fn clap_audio(&self) -> Option<&SharedSession> {
        self.get(&ArtifactId::from_static("hf_audio"))
            .or_else(|| self.get(&ArtifactId::from_static("hf_audio.onnx")))
    }

    pub fn clap_text(&self) -> Option<&SharedSession> {
        self.get(&ArtifactId::from_static("text_model"))
            .or_else(|| self.get(&ArtifactId::from_static("text_model.onnx")))
    }
}

/// A bounded pool of ONNX model sessions for concurrent CPU inference.
pub struct BoundedSessionPool {
    id: ArtifactId,
    sessions: Arc<Mutex<Vec<Session>>>,
    condvar: Arc<Condvar>,
    capacity: usize,
    _reservation: ResourceReservation,
    #[cfg(test)]
    wait_observer: Option<std::sync::mpsc::Sender<()>>,
}

/// Returns the checkout on success, error, and unwinding. User code never holds
/// the pool mutex. A poisoned shared-session mutex remains a fail-closed error.
struct SessionCheckout<'a> {
    pool: &'a BoundedSessionPool,
    session: Option<Session>,
}

impl Drop for SessionCheckout<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // Recover only to return ownership and wake waiters; checkout still
            // reports LockPoisoned rather than using a poisoned pool.
            let mut sessions = self.pool.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.push(session);
            self.pool.condvar.notify_all();
        }
    }
}

impl BoundedSessionPool {
    pub fn from_verified_bytes(
        artifact: &VerifiedBytes,
        pool_size: usize,
        intra_threads: Option<usize>,
    ) -> Result<Self, OnnxError> {
        let threads = validate_resources(pool_size, intra_threads)?;
        let reservation =
            ResourceReservation::acquire(pool_size, threads, artifact.as_bytes().len())?;

        initialize_runtime()?;
        let mut sessions = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let builder = bounded_session_builder(threads)?;

            let session = builder
                .commit_from_memory(artifact.as_bytes())
                .map_err(|e| OnnxError::SessionInit {
                    id: artifact.id(),
                    message: e.to_string(),
                })?;

            sessions.push(session);
        }

        Ok(Self {
            id: artifact.id(),
            sessions: Arc::new(Mutex::new(sessions)),
            condvar: Arc::new(Condvar::new()),
            capacity: pool_size,
            _reservation: reservation,
            #[cfg(test)]
            wait_observer: None,
        })
    }

    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn available(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Check out a session from the pool, run `f`, and return the session to the pool when done.
    pub fn with_session<F, R>(&self, f: F) -> Result<R, OnnxError>
    where
        F: FnOnce(&mut Session) -> Result<R, OnnxError>,
    {
        let session = {
            let mut guard = self.sessions.lock().map_err(|_| OnnxError::LockPoisoned)?;
            loop {
                if let Some(sess) = guard.pop() {
                    break sess;
                }
                #[cfg(test)]
                if let Some(observer) = &self.wait_observer {
                    let _ = observer.send(());
                }
                guard = self
                    .condvar
                    .wait(guard)
                    .map_err(|_| OnnxError::LockPoisoned)?;
            }
        };

        let mut checkout = SessionCheckout {
            pool: self,
            session: Some(session),
        };
        // The checkout owns a session until Drop takes it; no user operation
        // can remove it while this closure is running.
        f(checkout.session.as_mut().ok_or(OnnxError::LockPoisoned)?)
    }
}

/// Fixed CPU policy: 1 thread by default, never ORT's machine-sized auto pool.
fn validate_resources(pool_size: usize, threads: Option<usize>) -> Result<usize, OnnxError> {
    let threads = threads.unwrap_or(1);
    if !(1..=4).contains(&pool_size) || !(1..=4).contains(&threads) {
        return Err(OnnxError::SessionBuilder(
            "pool size and intra threads must each be in 1..=4".into(),
        ));
    }
    Ok(threads)
}

fn bounded_session_builder(
    threads: usize,
) -> Result<ort::session::builder::SessionBuilder, OnnxError> {
    Session::builder()
        .and_then(|builder| builder.with_intra_threads(threads))
        .and_then(|builder| builder.with_inter_threads(1))
        .and_then(|builder| builder.with_parallel_execution(false))
        .and_then(|builder| {
            builder.with_execution_providers([
                ort::execution_providers::CPUExecutionProvider::default()
                    .with_arena_allocator(false)
                    .build(),
            ])
        })
        .map_err(|e| OnnxError::SessionBuilder(e.to_string()))
}

/// Initialize only a separately provisioned, administrator-trusted runtime.
/// Never search the working directory or let ort select/download a library.
fn initialize_runtime() -> Result<(), OnnxError> {
    static RUNTIME: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let path = std::env::var("ORT_DYLIB_PATH").map_err(|_| {
                "ORT_DYLIB_PATH must name a provisioned absolute runtime path".to_string()
            })?;
            if !std::path::Path::new(&path).is_absolute()
                || !std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file())
            {
                return Err(
                    "ORT_DYLIB_PATH must name an existing absolute runtime file".to_string()
                );
            }
            // ort rc.10 panics on dlopen/API incompatibility instead of returning
            // an error. Contain that at the capability boundary, never in inference.
            std::panic::catch_unwind(|| ort::init_from(path).commit())
                .map_err(|_| {
                    "ORT_DYLIB_PATH runtime could not be loaded (library/API incompatible)"
                        .to_string()
                })?
                .map(|_| ())
                .map_err(|e| format!("ORT_DYLIB_PATH runtime initialization: {e}"))
        })
        .clone()
        .map_err(OnnxError::SessionBuilder)
}

/// A `ModelConsumer` that loads ONNX model sessions from `VerifiedBytes`.
#[derive(Debug, Clone, Default)]
pub struct OnnxModelConsumer {
    intra_threads: Option<usize>,
}

impl OnnxModelConsumer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_intra_threads(mut self, threads: usize) -> Self {
        self.intra_threads = Some(threads);
        self
    }
}

impl ModelConsumer for OnnxModelConsumer {
    type Output = SharedSession;
    type Error = OnnxError;

    fn load(&self, artifact: VerifiedBytes) -> Result<Self::Output, Self::Error> {
        initialize_runtime()?;
        let threads = validate_resources(1, self.intra_threads)?;
        let reservation = ResourceReservation::acquire(1, threads, artifact.as_bytes().len())?;
        let builder = bounded_session_builder(threads)?;

        let session = builder
            .commit_from_memory(artifact.as_bytes())
            .map_err(|e| OnnxError::SessionInit {
                id: artifact.id(),
                message: e.to_string(),
            })?;

        Ok(SharedSession::new(artifact.id(), session, reservation))
    }
}
