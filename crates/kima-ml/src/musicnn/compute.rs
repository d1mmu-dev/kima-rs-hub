//! DB-free computation only. No complete-analysis or availability assertion.
use std::collections::BTreeMap;

use super::{MusicnnFrontend, mel_patches};
use crate::VerifiedBytes;
use crate::onnx::{MusiCnnHead, MusicnnCoreSession, MusicnnHeadSession, OnnxError};

#[derive(Debug, thiserror::Error)]
pub enum MusicnnComputeError {
    #[error("MusiCNN frontend: {0}")]
    Frontend(&'static str),
    #[error(transparent)]
    Onnx(#[from] OnnxError),
    #[error("invalid MusiCNN patch outputs: {0}")]
    InvalidPatchOutputs(&'static str),
}

/// A computation outcome, not a stored analysis result. No full patch is not
/// a zero embedding, a neutral mood, or successful combined DSP analysis.
#[derive(Debug, PartialEq)]
pub enum MusicnnComputation {
    NoFullPatch,
    Patches(MusicnnPatchOutputs),
}

/// Raw patch-level model outputs in temporal order. No averaging, temperature
/// scaling, clamping, rounding, mood tags, DSP fields or persistence schema.
#[derive(Debug, PartialEq)]
pub struct MusicnnPatchOutputs {
    embeddings: Vec<[f32; 200]>,
    heads: BTreeMap<MusiCnnHead, Vec<[f32; 2]>>,
}

impl MusicnnPatchOutputs {
    /// Build directly from per-patch rows, e.g. to feed the pure aggregation
    /// function from independently captured real model outputs without
    /// running ONNX. Enforces the same invariants `MusicnnCompute::compute`
    /// already guarantees internally: nonempty, all ten heads present (no
    /// more, no fewer), every head's row count matching the patch count, and
    /// every value finite. This does not reach into `MusicnnComputation`:
    /// callers needing the `NoFullPatch` case still use that enum directly.
    pub fn try_new(
        embeddings: Vec<[f32; 200]>,
        heads: BTreeMap<MusiCnnHead, Vec<[f32; 2]>>,
    ) -> Result<Self, MusicnnComputeError> {
        if embeddings.is_empty() {
            return Err(MusicnnComputeError::InvalidPatchOutputs(
                "no patches (use MusicnnComputation::NoFullPatch for the sub-patch case)",
            ));
        }
        if embeddings.iter().flatten().any(|x| !x.is_finite()) {
            return Err(MusicnnComputeError::InvalidPatchOutputs(
                "non-finite embedding value",
            ));
        }
        if heads.len() != MusiCnnHead::all().len()
            || MusiCnnHead::all().iter().any(|h| !heads.contains_key(h))
        {
            return Err(MusicnnComputeError::InvalidPatchOutputs(
                "must have exactly the ten pinned MusiCNN heads",
            ));
        }
        for rows in heads.values() {
            if rows.len() != embeddings.len() {
                return Err(MusicnnComputeError::InvalidPatchOutputs(
                    "head row count must match patch count",
                ));
            }
            if rows.iter().flatten().any(|x| !x.is_finite()) {
                return Err(MusicnnComputeError::InvalidPatchOutputs(
                    "non-finite head output value",
                ));
            }
        }
        Ok(Self { embeddings, heads })
    }

    pub fn embeddings(&self) -> &[[f32; 200]] {
        &self.embeddings
    }
    pub fn heads(&self) -> &BTreeMap<MusiCnnHead, Vec<[f32; 2]>> {
        &self.heads
    }
}

/// All eleven pinned models are mandatory (binding spec). CPU group size is
/// one, as in pinned MusicnnBody; no per-head degraded success is returned.
/// Initialize once and reuse; native session admission limits still apply.
pub struct MusicnnCompute {
    frontend: MusicnnFrontend,
    core: MusicnnCoreSession,
    heads: BTreeMap<MusiCnnHead, MusicnnHeadSession>,
}

impl MusicnnCompute {
    /// Load only owned verified bytes. Typed sessions recheck compiled-in pins,
    /// independent of any manifest or descriptor supplied by the asset writer.
    pub fn from_verified_artifacts(
        core: &VerifiedBytes,
        heads: &BTreeMap<MusiCnnHead, VerifiedBytes>,
    ) -> Result<Self, MusicnnComputeError> {
        for head in MusiCnnHead::all() {
            if !heads.contains_key(head) {
                return Err(OnnxError::MissingHead(*head).into());
            }
        }
        let core = MusicnnCoreSession::from_verified_bytes(core, 1, Some(1))?;
        let heads = MusiCnnHead::all()
            .iter()
            .map(|&head| {
                MusicnnHeadSession::from_verified_bytes(&heads[&head], head, 1, Some(1))
                    .map(|session| (head, session))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            frontend: MusicnnFrontend::new(),
            core,
            heads,
        })
    }

    /// Caller supplies the existing mono 16 kHz final <=90s tail. Does not
    /// decode/resample, clip signal values, detect silence or publish anything.
    pub fn compute(
        &mut self,
        tail_mono_16k: &[f32],
    ) -> Result<MusicnnComputation, MusicnnComputeError> {
        let frames = self
            .frontend
            .compute(tail_mono_16k)
            .map_err(MusicnnComputeError::Frontend)?;
        run_batches(
            &frames,
            |flat, batch| self.core.infer(flat, batch),
            |head, embeddings| self.heads[&head].infer(embeddings),
        )
    }
}

fn run_batches(
    frames: &[[f32; 96]],
    mut core: impl FnMut(&[f32], usize) -> Result<Vec<[f32; 200]>, OnnxError>,
    mut head: impl FnMut(MusiCnnHead, &[[f32; 200]]) -> Result<Vec<[f32; 2]>, OnnxError>,
) -> Result<MusicnnComputation, MusicnnComputeError> {
    let mut embeddings = Vec::new();
    let mut heads: BTreeMap<_, Vec<_>> = MusiCnnHead::all()
        .iter()
        .map(|&h| (h, Vec::new()))
        .collect();
    for patch in mel_patches(frames) {
        let flat: Vec<f32> = patch.iter().flatten().copied().collect();
        let rows = core(&flat, 1)?;
        if rows.len() != 1 || rows.iter().flatten().any(|x| !x.is_finite()) {
            return Err(OnnxError::Inference("invalid MusiCNN core batch output".into()).into());
        }
        for (&h, output) in &mut heads {
            let predictions = head(h, &rows)?;
            if predictions.len() != 1 || predictions.iter().flatten().any(|x| !x.is_finite()) {
                return Err(
                    OnnxError::Inference(format!("invalid MusiCNN {h:?} batch output")).into(),
                );
            }
            output.extend(predictions);
        }
        embeddings.extend(rows);
    }
    if embeddings.is_empty() {
        Ok(MusicnnComputation::NoFullPatch)
    } else {
        Ok(MusicnnComputation::Patches(MusicnnPatchOutputs::try_new(
            embeddings, heads,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_row_counts_and_nonfinite_backend_outputs_without_partial_success() {
        let frames = vec![[0.25; 96]; 280];
        for core_rows in [vec![], vec![[1.0; 200]; 2], vec![[f32::NAN; 200]]] {
            assert!(
                run_batches(
                    &frames,
                    |_, _| Ok(core_rows.clone()),
                    |_, rows| Ok(vec![[0.2, 0.8]; rows.len()])
                )
                .is_err()
            );
        }
        for head_rows in [vec![], vec![[0.2, 0.8]; 2], vec![[f32::INFINITY, 0.0]]] {
            assert!(
                run_batches(
                    &frames,
                    |_, _| Ok(vec![[1.0; 200]]),
                    |_, _| Ok(head_rows.clone())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn nonconstant_signal_reaches_all_ten_heads_in_patch_order_with_cpu_batch_one() {
        let samples: Vec<_> = (0..373 * 256)
            .map(|i| ((i % 17) as f32 - 8.0) / 16.0)
            .collect();
        let frames = super::super::MusicnnFrontend::new()
            .compute(&samples)
            .unwrap();
        let expected: Vec<_> = super::super::mel_patches(&frames)
            .map(|patch| patch.iter().flatten().copied().collect::<Vec<_>>())
            .collect();
        let mut calls = 0;
        let mut head_calls = Vec::new();
        let got = run_batches(
            &frames,
            |flat, batch| {
                assert_eq!(batch, 1);
                assert_eq!(flat, &expected[calls]);
                assert!(flat.iter().any(|&x| x > 0.0));
                calls += 1;
                Ok(vec![[calls as f32; 200]])
            },
            |head, embeddings| {
                assert_eq!(embeddings.len(), 1);
                head_calls.push(head);
                Ok(vec![[embeddings[0][0], head_calls.len() as f32]])
            },
        )
        .unwrap();
        let MusicnnComputation::Patches(got) = got else {
            panic!("missing patches")
        };
        assert_eq!(calls, 3);
        assert_eq!(
            got.embeddings().iter().map(|r| r[0]).collect::<Vec<_>>(),
            [1.0, 2.0, 3.0]
        );
        assert_eq!(got.heads().len(), 10);
        for head in MusiCnnHead::all() {
            assert_eq!(head_calls.iter().filter(|h| *h == head).count(), 3);
            assert_eq!(
                got.heads()[head].iter().map(|r| r[0]).collect::<Vec<_>>(),
                [1.0, 2.0, 3.0]
            );
        }
    }
}
