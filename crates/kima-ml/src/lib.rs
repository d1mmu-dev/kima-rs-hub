//! kima-ml: Model integrity verification, capability evaluation, and ML runtime.

pub mod audio;
pub mod musicnn;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::sync::LazyLock;

use sha2::{Digest, Sha256};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ArtifactId(Cow<'static, str>);

impl ArtifactId {
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    pub id: ArtifactId,
    pub basename: &'static str,
    pub expected_sha256: [u8; 32],
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBytes {
    id: ArtifactId,
    bytes: Box<[u8]>,
}

impl VerifiedBytes {
    pub fn id(&self) -> ArtifactId {
        self.id.clone()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Box<[u8]> {
        self.bytes
    }
}

pub trait ModelConsumer {
    type Output;
    type Error;

    fn load(&self, artifact: VerifiedBytes) -> Result<Self::Output, Self::Error>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum ArtifactError {
    #[error("artifact {id}: file missing")]
    Missing { id: ArtifactId },

    #[error("artifact {id}: file not regular")]
    NotRegular { id: ArtifactId },

    #[error("artifact {id}: symlink refused")]
    SymlinkRefused { id: ArtifactId },

    #[error("artifact {id}: permission denied")]
    PermissionDenied { id: ArtifactId },

    #[error("artifact {id}: file too large (exceeds {max_bytes} bytes)")]
    TooLarge { id: ArtifactId, max_bytes: u64 },

    #[error("artifact {id}: hash mismatch (expected {expected_hex}, actual {actual_hex})")]
    HashMismatch {
        id: ArtifactId,
        expected_hex: String,
        actual_hex: String,
    },

    #[error("artifact {id}: unsafe basename {basename:?}")]
    UnsafeBasename { id: ArtifactId, basename: String },

    #[error("artifact {id}: platform unsupported")]
    UnsupportedPlatform { id: ArtifactId },

    #[error("artifact {id}: I/O error ({errno})")]
    Io { id: ArtifactId, errno: i32 },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    MusicnnAnalysis,
    ClapAudioEmbedding,
    ClapTextEmbedding,
    Dsp,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Missing,
    NotRegular,
    SymlinkRefused,
    PermissionDenied,
    TooLarge,
    HashMismatch,
    UnsafeBasename,
    UnsupportedPlatform,
    ConsumerLoadFailed,
    Io,
}

impl From<&ArtifactError> for FailureCategory {
    fn from(err: &ArtifactError) -> Self {
        match err {
            ArtifactError::Missing { .. } => FailureCategory::Missing,
            ArtifactError::NotRegular { .. } => FailureCategory::NotRegular,
            ArtifactError::SymlinkRefused { .. } => FailureCategory::SymlinkRefused,
            ArtifactError::PermissionDenied { .. } => FailureCategory::PermissionDenied,
            ArtifactError::TooLarge { .. } => FailureCategory::TooLarge,
            ArtifactError::HashMismatch { .. } => FailureCategory::HashMismatch,
            ArtifactError::UnsafeBasename { .. } => FailureCategory::UnsafeBasename,
            ArtifactError::UnsupportedPlatform { .. } => FailureCategory::UnsupportedPlatform,
            ArtifactError::Io { .. } => FailureCategory::Io,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactFailure {
    pub artifact_id: ArtifactId,
    pub category: FailureCategory,
    pub message: String,
}

impl From<ArtifactError> for ArtifactFailure {
    fn from(err: ArtifactError) -> Self {
        let artifact_id = match &err {
            ArtifactError::Missing { id }
            | ArtifactError::NotRegular { id }
            | ArtifactError::SymlinkRefused { id }
            | ArtifactError::PermissionDenied { id }
            | ArtifactError::TooLarge { id, .. }
            | ArtifactError::HashMismatch { id, .. }
            | ArtifactError::UnsafeBasename { id, .. }
            | ArtifactError::UnsupportedPlatform { id }
            | ArtifactError::Io { id, .. } => id.clone(),
        };
        let category = FailureCategory::from(&err);
        let message = err.to_string();
        Self {
            artifact_id,
            category,
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityStatus {
    Available,
    Disabled { reason: String },
    Unavailable { errors: Vec<ArtifactFailure> },
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityConfig {
    pub disable_clap: bool,
}

static NOT_EVALUATED_STATUS: LazyLock<CapabilityStatus> =
    LazyLock::new(|| CapabilityStatus::Unavailable {
        errors: vec![ArtifactFailure {
            artifact_id: ArtifactId::from_static("unknown"),
            category: FailureCategory::Missing,
            message: "capability not evaluated".to_string(),
        }],
    });

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapabilityEvaluationReport {
    pub capabilities: BTreeMap<Capability, CapabilityStatus>,
}

impl CapabilityEvaluationReport {
    pub fn status(&self, cap: Capability) -> &CapabilityStatus {
        self.capabilities.get(&cap).unwrap_or(&NOT_EVALUATED_STATUS)
    }

    pub fn is_available(&self, cap: Capability) -> bool {
        matches!(
            self.capabilities.get(&cap),
            Some(CapabilityStatus::Available)
        )
    }
}

pub fn evaluate_capabilities(
    model_dir: &Path,
    config: &CapabilityConfig,
    requirements: &BTreeMap<Capability, Vec<ArtifactDescriptor>>,
) -> CapabilityEvaluationReport {
    evaluate_capabilities_with_loader(model_dir, config, requirements, |_cap, _bytes| {
        Ok::<(), std::convert::Infallible>(())
    })
}

pub fn evaluate_capabilities_with_loader<F, E>(
    model_dir: &Path,
    config: &CapabilityConfig,
    requirements: &BTreeMap<Capability, Vec<ArtifactDescriptor>>,
    mut loader: F,
) -> CapabilityEvaluationReport
where
    F: FnMut(Capability, VerifiedBytes) -> Result<(), E>,
    E: std::fmt::Display,
{
    let mut capabilities = BTreeMap::new();

    for (&cap, descriptors) in requirements {
        if config.disable_clap
            && (cap == Capability::ClapAudioEmbedding || cap == Capability::ClapTextEmbedding)
        {
            capabilities.insert(
                cap,
                CapabilityStatus::Disabled {
                    reason: "DISABLE_CLAP is active".to_string(),
                },
            );
            continue;
        }

        let mut errors = Vec::new();
        for desc in descriptors {
            match verify_artifact(model_dir, desc) {
                Ok(verified) => {
                    let id = verified.id();
                    if let Err(load_err) = loader(cap, verified) {
                        errors.push(ArtifactFailure {
                            artifact_id: id,
                            category: FailureCategory::ConsumerLoadFailed,
                            message: load_err.to_string(),
                        });
                    }
                }
                Err(e) => {
                    errors.push(ArtifactFailure::from(e));
                }
            }
        }

        if errors.is_empty() {
            capabilities.insert(cap, CapabilityStatus::Available);
        } else {
            capabilities.insert(cap, CapabilityStatus::Unavailable { errors });
        }
    }

    CapabilityEvaluationReport { capabilities }
}

fn validate_basename(id: &ArtifactId, basename: &str) -> Result<(), ArtifactError> {
    if basename.is_empty()
        || basename == "."
        || basename == ".."
        || basename.contains('/')
        || basename.contains('\\')
        || !basename.is_ascii()
    {
        return Err(ArtifactError::UnsafeBasename {
            id: id.clone(),
            basename: basename.to_string(),
        });
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

#[cfg(unix)]
pub fn verify_artifact(
    model_dir: &Path,
    descriptor: &ArtifactDescriptor,
) -> Result<VerifiedBytes, ArtifactError> {
    validate_basename(&descriptor.id, descriptor.basename)?;

    let target_path = model_dir.join(descriptor.basename);
    let c_path = CString::new(target_path.as_os_str().as_bytes()).map_err(|_| {
        ArtifactError::UnsafeBasename {
            id: descriptor.id.clone(),
            basename: descriptor.basename.to_string(),
        }
    })?;

    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };

    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::ENOENT) => Err(ArtifactError::Missing {
                id: descriptor.id.clone(),
            }),
            Some(libc::ELOOP) => Err(ArtifactError::SymlinkRefused {
                id: descriptor.id.clone(),
            }),
            Some(libc::EACCES) | Some(libc::EPERM) => Err(ArtifactError::PermissionDenied {
                id: descriptor.id.clone(),
            }),
            Some(code) => Err(ArtifactError::Io {
                id: descriptor.id.clone(),
                errno: code,
            }),
            None => Err(ArtifactError::Io {
                id: descriptor.id.clone(),
                errno: 0,
            }),
        };
    }

    // Inspect file handle
    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    let stat_res = unsafe { libc::fstat(fd, &mut stat_buf) };
    if stat_res != 0 {
        unsafe { libc::close(fd) };
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(ArtifactError::Io {
            id: descriptor.id.clone(),
            errno: err,
        });
    }

    let file_type = stat_buf.st_mode & libc::S_IFMT;
    if file_type != libc::S_IFREG {
        unsafe { libc::close(fd) };
        return Err(ArtifactError::NotRegular {
            id: descriptor.id.clone(),
        });
    }

    // Wrap fd in File so it is closed automatically on drop
    let file = unsafe { File::from_raw_fd(fd) };

    let mut buffer = Vec::new();
    let mut reader = file.take(descriptor.max_bytes.saturating_add(1));
    reader
        .read_to_end(&mut buffer)
        .map_err(|e| ArtifactError::Io {
            id: descriptor.id.clone(),
            errno: e.raw_os_error().unwrap_or(0),
        })?;

    if buffer.len() as u64 > descriptor.max_bytes {
        return Err(ArtifactError::TooLarge {
            id: descriptor.id.clone(),
            max_bytes: descriptor.max_bytes,
        });
    }

    let actual_hash: [u8; 32] = Sha256::digest(&buffer).into();
    if actual_hash != descriptor.expected_sha256 {
        return Err(ArtifactError::HashMismatch {
            id: descriptor.id.clone(),
            expected_hex: to_hex(&descriptor.expected_sha256),
            actual_hex: to_hex(&actual_hash),
        });
    }

    Ok(VerifiedBytes {
        id: descriptor.id.clone(),
        bytes: buffer.into_boxed_slice(),
    })
}

#[cfg(not(unix))]
pub fn verify_artifact(
    _model_dir: &Path,
    descriptor: &ArtifactDescriptor,
) -> Result<VerifiedBytes, ArtifactError> {
    validate_basename(&descriptor.id, descriptor.basename)?;
    Err(ArtifactError::UnsupportedPlatform {
        id: descriptor.id.clone(),
    })
}
