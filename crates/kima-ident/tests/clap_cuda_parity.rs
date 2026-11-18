//! Private CUDA-vs-CPU parity gate for the music CLAP encoders (ignored: the noncommercial
//! weights and the CUDA runtime are never in Git or CI).
//!
//! Loads the pinned `music_audio.onnx` / `music_text.onnx` / `tokenizer.json` through the
//! production adapters twice — once on the CPU execution provider, once on CUDA — and
//! compares, per synthetic audio window, per aggregated five-window track vector and per
//! zero-shot reading prompt (upstream's 24), the cosine of the unit-normalized outputs.
//! Gate: every cosine >= 0.999 (target ~0.99999). The CUDA side must really run on CUDA:
//! a CPU fallback fails the test instead of passing trivially.
//!
//! Environment (every gap panics, so a skip can never read as a pass):
//! - `KIMA_CLAP_MODEL_DIR`: absolute directory with the three pinned files.
//! - `ORT_DYLIB_PATH`: a CUDA-enabled ONNX Runtime 1.22 (`libonnxruntime.so` with
//!   `libonnxruntime_providers_{shared,cuda}.so` beside it).
//! - `KIMA_CUDA_LIB_DIR` (optional): CUDA 12 / cuDNN 9 libraries to preload.
//! - `KIMA_PARITY_RECORD` (optional): path of a JSON run record to write.
use kima_ml::clap::{MusicClapMel, normalize_embedding, postprocess_clap_model_outputs};
use kima_ml::onnx::{
    ActiveProvider, ClapAudioSession, ClapTextSession, ExecutionProvider, OnnxModel, ProviderConfig,
};
use kima_ml::text_tokenizer::{self, ClapTextTokenizer};

const GATE: f64 = 0.999;

fn env_path(key: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set")),
    );
    assert!(path.is_absolute(), "{key} must be absolute");
    path
}

fn cuda() -> ProviderConfig {
    ProviderConfig {
        provider: ExecutionProvider::Cuda,
        cuda_lib_dir: std::env::var_os("KIMA_CUDA_LIB_DIR").map(Into::into),
        ..ProviderConfig::default()
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut x, mut y) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&p, &q) in a.iter().zip(b) {
        let (p, q) = (f64::from(p), f64::from(q));
        dot += p * q;
        x += p * p;
        y += q * q;
    }
    dot / (x.sqrt() * y.sqrt())
}

/// Deterministic 10 s, 48 kHz synthetic windows (no RNG crate; LCG noise).
fn signals() -> Vec<(String, Vec<f32>)> {
    let lcg = |seed: u64| {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    };
    let n = 480_000;
    let mut out = Vec::new();
    for (i, f) in [110.0_f32, 220.0, 440.0, 880.0, 1760.0].iter().enumerate() {
        let mut noise = lcg(i as u64 + 1);
        out.push((
            format!("tone{f}+noise"),
            (0..n)
                .map(|t| {
                    0.3 * (2.0 * std::f32::consts::PI * f * t as f32 / 48_000.0).sin()
                        + 0.05 * noise()
                })
                .collect(),
        ));
    }
    out.push((
        "chirp".into(),
        (0..n)
            .map(|t| {
                let s = t as f32 / 48_000.0;
                0.4 * (2.0 * std::f32::consts::PI * (50.0 * s + 400.0 * s * s)).sin()
            })
            .collect(),
    ));
    let mut noise = lcg(99);
    out.push(("white".into(), (0..n).map(|_| 0.25 * noise()).collect()));
    out.push((
        "drone-pulse".into(),
        (0..n)
            .map(|t| {
                let s = t as f32 / 48_000.0;
                let env = if (s * 2.0).fract() < 0.1 { 1.0 } else { 0.2 };
                env * (0.3 * (2.0 * std::f32::consts::PI * 55.0 * s).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * 82.5 * s).sin())
            })
            .collect(),
    ));
    out
}

#[test]
#[ignore = "private noncommercial music CLAP models and a CUDA ONNX Runtime required"]
fn cuda_music_clap_matches_cpu_audio_and_text() {
    let dir = env_path("KIMA_CLAP_MODEL_DIR");
    let ort = env_path("ORT_DYLIB_PATH");

    // Audio.
    let artifact = kima_ml::verify_artifact(&dir, &OnnxModel::ClapAudio.descriptor()).unwrap();
    let cpu = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(4)).unwrap();
    let gpu = ClapAudioSession::from_verified_bytes_on(&artifact, 1, Some(1), &cuda()).unwrap();
    assert_eq!(cpu.provider(), &ActiveProvider::Cpu);
    assert!(
        gpu.provider().is_cuda(),
        "CUDA side fell back: {:?}",
        gpu.provider()
    );
    let mel = MusicClapMel::new();
    let mut audio = Vec::new();
    let (mut cpu_raw, mut gpu_raw) = (Vec::new(), Vec::new());
    let mut cpu_ms = 0.0;
    let mut gpu_ms = 0.0;
    for (name, samples) in signals() {
        let frames: Vec<f32> = mel
            .compute(&samples)
            .unwrap()
            .into_iter()
            .flatten()
            .collect();
        let t = std::time::Instant::now();
        let c = cpu.infer(&frames, 1).unwrap()[0];
        cpu_ms += t.elapsed().as_secs_f64() * 1e3;
        let t = std::time::Instant::now();
        let g = gpu.infer(&frames, 1).unwrap()[0];
        gpu_ms += t.elapsed().as_secs_f64() * 1e3;
        let cos = cosine(
            &normalize_embedding(&c).unwrap(),
            &normalize_embedding(&g).unwrap(),
        );
        println!("audio window {name}: cosine={cos:.9}");
        audio.push(serde_json::json!({"signal": name, "cosine": cos}));
        cpu_raw.push(c);
        gpu_raw.push(g);
    }
    // Five-window track vectors, as the embedding stage aggregates them.
    let mut tracks = Vec::new();
    for (i, (c, g)) in cpu_raw.windows(5).zip(gpu_raw.windows(5)).enumerate() {
        let c = postprocess_clap_model_outputs(c).unwrap();
        let g = postprocess_clap_model_outputs(g).unwrap();
        let cos = cosine(c.as_slice(), g.as_slice());
        println!("track vector {i}: cosine={cos:.9}");
        tracks.push(cos);
    }
    // Batched CUDA (5 windows in one call) against the CPU single-window outputs.
    let batch: Vec<f32> = signals()
        .into_iter()
        .take(5)
        .flat_map(|(_, s)| mel.compute(&s).unwrap().into_iter().flatten())
        .collect();
    let batched = gpu.infer(&batch, 5).unwrap();
    let batched_min = batched
        .iter()
        .zip(&cpu_raw)
        .map(|(g, c)| {
            cosine(
                &normalize_embedding(c).unwrap(),
                &normalize_embedding(g).unwrap(),
            )
        })
        .fold(1.0_f64, f64::min);
    println!("audio batch5 vs cpu single: min cosine={batched_min:.9}");
    drop((cpu, gpu));

    // Text: upstream's 24 zero-shot reading prompts.
    let tokenizer = ClapTextTokenizer::from_verified_bytes(
        &kima_ml::verify_artifact(&dir, &text_tokenizer::descriptor()).unwrap(),
    )
    .unwrap();
    let text = kima_ml::verify_artifact(&dir, &OnnxModel::ClapText.descriptor()).unwrap();
    let cpu = ClapTextSession::from_verified_bytes(&text, 1, Some(4)).unwrap();
    let gpu = ClapTextSession::from_verified_bytes_on(&text, 1, Some(1), &cuda()).unwrap();
    assert!(
        gpu.provider().is_cuda(),
        "CUDA text side fell back: {:?}",
        gpu.provider()
    );
    let prompts: Vec<&str> = kima_vibe::zero_shot::PROMPTS
        .iter()
        .flat_map(|(_, p)| p.iter().copied())
        .collect();
    assert_eq!(prompts.len(), 24);
    let mut text_cos = Vec::new();
    for prompt in &prompts {
        let (ids, mask) = tokenizer.encode_padded(prompt).unwrap();
        let c = cpu.infer(&ids, &mask, 1).unwrap()[0];
        let g = gpu.infer(&ids, &mask, 1).unwrap()[0];
        let cos = cosine(
            &normalize_embedding(&c).unwrap(),
            &normalize_embedding(&g).unwrap(),
        );
        println!("text prompt {prompt:?}: cosine={cos:.9}");
        text_cos.push(serde_json::json!({"prompt": prompt, "cosine": cos}));
    }

    let min = |v: &[serde_json::Value]| {
        v.iter()
            .map(|x| x["cosine"].as_f64().unwrap())
            .fold(1.0_f64, f64::min)
    };
    let audio_min = min(&audio);
    let text_min = min(&text_cos);
    let track_min = tracks.iter().copied().fold(1.0_f64, f64::min);
    println!(
        "worst: audio window {audio_min:.9}, track {track_min:.9}, batch5 {batched_min:.9}, text {text_min:.9}; \
         audio infer total cpu(4 threads) {cpu_ms:.0} ms vs cuda {gpu_ms:.0} ms over {} windows",
        audio.len()
    );
    if let Some(path) = std::env::var_os("KIMA_PARITY_RECORD") {
        let record = serde_json::json!({
            "gate": GATE,
            "ort_dylib": ort,
            "audio_model_sha256": OnnxModel::ClapAudio.expected_sha256().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "text_model_sha256": OnnxModel::ClapText.expected_sha256().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "audio_windows": audio,
            "track_vectors": tracks,
            "audio_batch5_min": batched_min,
            "text_prompts": text_cos,
            "worst": {"audio_window": audio_min, "track": track_min, "batch5": batched_min, "text": text_min},
            "timing_ms": {"cpu_4threads_total": cpu_ms, "cuda_total": gpu_ms, "windows": cpu_raw.len()},
        });
        std::fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }
    for (what, value) in [
        ("audio window", audio_min),
        ("track vector", track_min),
        ("batch5", batched_min),
        ("text prompt", text_min),
    ] {
        assert!(
            value >= GATE,
            "{what} cosine {value:.9} below the {GATE} gate"
        );
    }
}
