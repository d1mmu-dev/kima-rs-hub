//! Scratch CLAP performance bench (perf investigation only, not for commit).
//!
//! Modes (all print one JSON line per measurement to stdout):
//!   stages   <file>                          production code path, pool 1 / threads 1, per-stage timing
//!   rawdecode <file>                         symphonia packet decode only (no mono/sha/resample)
//!   micro                                    sha2 per-sample cost, rubato resample_mono of 90 s, soxr, mel
//!   onnx  <threads> <sessions> <windows> <batch>   ONNX throughput sweep (raw ort sessions)
//!   pipe  <file> <workers> <threads> <copies>      end-to-end parallel tracks (decode+prep+mel+infer)
//!
//! Env: ORT_DYLIB_PATH, KIMA_CLAP_MODEL_DIR.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use kima_ml::audio::decode_clap_auto;
use kima_ml::clap::{MusicClapMel, postprocess_clap_model_outputs, prepare_window};
use kima_ml::onnx::{ClapAudioSession, OnnxModel};
use kima_ml::{Capability, CapabilityConfig, VerifiedBytes, evaluate_capabilities_with_loader};
use serde_json::json;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSource;

fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn model_bytes() -> VerifiedBytes {
    let dir = PathBuf::from(std::env::var("KIMA_CLAP_MODEL_DIR").expect("KIMA_CLAP_MODEL_DIR"));
    let mut out = None;
    let req = BTreeMap::from([(
        Capability::ClapAudioEmbedding,
        vec![OnnxModel::ClapAudio.descriptor()],
    )]);
    let report =
        evaluate_capabilities_with_loader(&dir, &CapabilityConfig::default(), &req, |_, b| {
            out = Some(b);
            Ok::<(), String>(())
        });
    out.unwrap_or_else(|| panic!("model not verified: {report:?}"))
}

fn prod_session(bytes: &VerifiedBytes, pool: usize, threads: usize) -> ClapAudioSession {
    ClapAudioSession::from_verified_bytes(bytes, pool, Some(threads)).expect("session")
}

fn raw_session(bytes: &VerifiedBytes, threads: usize) -> ort::session::Session {
    if std::env::var("BENCH_CUDA").is_ok() {
        if let Ok(dir) = std::env::var("KIMA_CUDA_LIB_DIR") {
            static PRE: std::sync::Once = std::sync::Once::new();
            PRE.call_once(|| {
                ort::execution_providers::cuda::preload_dylibs(
                    Some(Path::new(&dir)),
                    Some(Path::new(&dir)),
                )
                .expect("preload")
            });
        }
        return ort::session::Session::builder()
            .and_then(|b| b.with_intra_threads(threads))
            .and_then(|b| {
                b.with_execution_providers([
                    ort::execution_providers::CUDAExecutionProvider::default()
                        .build()
                        .error_on_failure(),
                ])
            })
            .and_then(|b| b.commit_from_memory(bytes.as_bytes()))
            .expect("cuda session");
    }
    ort::session::Session::builder()
        .and_then(|b| b.with_intra_threads(threads))
        .and_then(|b| b.with_inter_threads(1))
        .and_then(|b| b.with_parallel_execution(false))
        .and_then(|b| {
            b.with_execution_providers([ort::execution_providers::CPUExecutionProvider::default()
                .with_arena_allocator(false)
                .build()])
        })
        .and_then(|b| b.commit_from_memory(bytes.as_bytes()))
        .expect("raw session")
}

fn raw_infer(session: &mut ort::session::Session, mel: &[f32], batch: usize) -> Vec<f32> {
    let tensor =
        ort::value::Tensor::from_array(([batch, 1, 1001, 64], mel.to_vec().into_boxed_slice()))
            .unwrap();
    let outputs = session.run(ort::inputs![tensor]).unwrap();
    outputs[0].try_extract_tensor::<f32>().unwrap().1.to_vec()
}

fn opener(
    path: &Path,
) -> impl FnMut() -> Result<Box<dyn MediaSource>, kima_ml::audio::AudioError> + '_ {
    move || Ok(Box::new(std::fs::File::open(path)?) as Box<dyn MediaSource>)
}

fn hint_for(path: &Path) -> Hint {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    hint
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn rawdecode(path: &Path) -> (f64, usize, u32) {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    let t = Instant::now();
    let mss = MediaSourceStream::new(
        Box::new(std::fs::File::open(path).unwrap()),
        Default::default(),
    );
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint_for(path),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .unwrap()
        .clone();
    let rate = params.sample_rate.unwrap();
    let id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .unwrap();
    let mut frames = 0usize;
    let mut buf: Vec<f32> = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id != id {
            continue;
        }
        let audio = decoder.decode(&packet).unwrap();
        buf.resize(audio.samples_interleaved(), 0.0);
        audio.copy_to_slice_interleaved(&mut buf);
        frames += audio.frames();
    }
    (ms(t), frames, rate)
}

fn stages(path: &Path) {
    let bytes = model_bytes();
    let t = Instant::now();
    let session = prod_session(&bytes, 1, 1);
    let load_ms = ms(t);
    let mel = MusicClapMel::new();
    let (raw_ms, frames, rate) = rawdecode(path);
    let duration = frames as f64 / f64::from(rate);
    let t = Instant::now();
    let (_audio, windows, rate) = decode_clap_auto(opener(path), &hint_for(path), None).unwrap();
    let decode_ms = ms(t);
    let (mut prep, mut melt, mut inf) = (0.0, 0.0, 0.0);
    let mut outs = Vec::new();
    for w in &windows {
        let t = Instant::now();
        let p = prepare_window(w, rate).unwrap();
        prep += ms(t);
        let t = Instant::now();
        let frames = mel.compute(&p).unwrap();
        melt += ms(t);
        let flat: Vec<f32> = frames.iter().flatten().copied().collect();
        let t = Instant::now();
        let o = session.infer(&flat, 1).unwrap();
        inf += ms(t);
        let mut v = [0f32; 512];
        v.copy_from_slice(&o[0]);
        outs.push(v);
    }
    let emb = postprocess_clap_model_outputs(&outs).unwrap();
    let total = decode_ms + prep + melt + inf;
    println!(
        "{}",
        json!({"mode":"stages","profile":profile(),"file":path.file_name().unwrap().to_str(),
            "duration_s":duration,"rate":rate,"windows":windows.len(),"session_load_ms":load_ms,
            "raw_symphonia_decode_1pass_ms":raw_ms,"decode_clap_auto_ms":decode_ms,
            "soxr_prepare_ms":prep,"music_mel_ms":melt,"onnx_infer_ms":inf,
            "track_total_ms":total,"realtime_factor":duration/(total/1000.0),
            "emb_head":&emb.as_slice()[..4]})
    );
}

fn micro() {
    use sha2::{Digest, Sha256};
    let n = 44_100 * 300; // 5 min mono frames
    let samples: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.001).sin() * 0.3).collect();
    let t = Instant::now();
    let mut d = Sha256::new();
    for s in &samples {
        d.update(s.to_bits().to_le_bytes());
    }
    let _ = d.finalize();
    let sha_per_sample = ms(t);
    let t = Instant::now();
    let mut d = Sha256::new();
    for s in &samples {
        if !s.is_finite() {
            panic!()
        }
    }
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    d.update(&bytes);
    let _ = d.finalize();
    let sha_bulk = ms(t);
    let ninety: Vec<f32> = samples[..44_100 * 90].to_vec();
    let t = Instant::now();
    let _ = kima_ml::audio::resample_mono(&ninety, 44_100, 16_000).unwrap();
    let r16 = ms(t);
    let t = Instant::now();
    let _ = kima_ml::audio::resample_mono(&ninety, 44_100, 48_000).unwrap();
    let r48 = ms(t);
    let win: Vec<f32> = samples[..441_000].to_vec();
    let t = Instant::now();
    let p = prepare_window(&win, 44_100).unwrap();
    let soxr = ms(t);
    let mel = MusicClapMel::new();
    let t = Instant::now();
    let _ = mel.compute(&p).unwrap();
    let melt = ms(t);
    println!(
        "{}",
        json!({"mode":"micro","profile":profile(),"frames":n,
            "sha256_per_sample_update_5min_ms":sha_per_sample,"sha256_bulk_5min_ms":sha_bulk,
            "rubato_90s_to16k_ms":r16,"rubato_90s_to48k_ms":r48,
            "soxr_one_window_ms":soxr,"music_mel_one_window_ms":melt})
    );
}

fn synthetic_mel() -> Vec<f32> {
    let win: Vec<f32> = (0..441_000)
        .map(|i| ((i as f32) * 0.01).sin() * 0.3)
        .collect();
    let p = prepare_window(&win, 44_100).unwrap();
    MusicClapMel::new()
        .compute(&p)
        .unwrap()
        .iter()
        .flatten()
        .copied()
        .collect()
}

fn onnx(threads: usize, sessions: usize, windows: usize, batch: usize) {
    let bytes = model_bytes();
    // Initializes ORT_DYLIB_PATH through the production loader first.
    let _prod = prod_session(&bytes, 1, 1);
    let mel1 = synthetic_mel();
    let mel: Vec<f32> = mel1
        .iter()
        .cycle()
        .take(mel1.len() * batch)
        .copied()
        .collect();
    let mut sess: Vec<_> = (0..sessions)
        .map(|_| raw_session(&bytes, threads))
        .collect();
    // warm-up
    for s in &mut sess {
        raw_infer(s, &mel, batch);
    }
    let calls = windows.div_ceil(batch);
    let lat = Arc::new(Mutex::new(Vec::new()));
    let t = Instant::now();
    std::thread::scope(|scope| {
        for s in sess.iter_mut() {
            let lat = lat.clone();
            let mel = &mel;
            scope.spawn(move || {
                for _ in 0..calls {
                    let t = Instant::now();
                    raw_infer(s, mel, batch);
                    lat.lock().unwrap().push(ms(t));
                }
            });
        }
    });
    let wall = t.elapsed().as_secs_f64();
    let lat = lat.lock().unwrap();
    let total_windows = (calls * batch * sessions) as f64;
    let mean = lat.iter().sum::<f64>() / lat.len() as f64;
    println!(
        "{}",
        json!({"mode":"onnx","profile":profile(),"intra_threads":threads,"sessions":sessions,
            "batch":batch,"windows_total":total_windows,"wall_s":wall,
            "windows_per_s":total_windows/wall,"mean_call_ms":mean,
            "ms_per_window_per_session":mean/batch as f64})
    );
}

fn pipe(path: &Path, workers: usize, threads: usize, copies: usize) {
    let bytes = model_bytes();
    let _prod = prod_session(&bytes, 1, 1);
    let nsess: usize = std::env::var("BENCH_SESSIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(workers);
    let sessions: Vec<_> = (0..nsess)
        .map(|_| Mutex::new(raw_session(&bytes, threads)))
        .collect();
    let mel = MusicClapMel::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let per = Mutex::new(Vec::new());
    let t = Instant::now();
    std::thread::scope(|scope| {
        for w in 0..workers {
            let s = &sessions[w % nsess];
            let (mel, next, per) = (&mel, &next, &per);
            scope.spawn(move || {
                loop {
                    if next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= copies {
                        break;
                    }
                    let t0 = Instant::now();
                    let (_a, windows, rate) =
                        decode_clap_auto(opener(path), &hint_for(path), None).unwrap();
                    let dec = ms(t0);
                    let t1 = Instant::now();
                    for w in &windows {
                        let p = prepare_window(w, rate).unwrap();
                        let flat: Vec<f32> =
                            mel.compute(&p).unwrap().iter().flatten().copied().collect();
                        raw_infer(&mut s.lock().unwrap(), &flat, 1);
                    }
                    per.lock().unwrap().push((dec, ms(t1), ms(t0)));
                }
            });
        }
    });
    let wall = t.elapsed().as_secs_f64();
    let vram = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let per = per.lock().unwrap();
    let n = per.len() as f64;
    let (_, frames, rate) = rawdecode(path);
    let dur = frames as f64 / f64::from(rate);
    println!(
        "{}",
        json!({"mode":"pipe","profile":profile(),"file":path.file_name().unwrap().to_str(),
            "duration_s":dur,"workers":workers,"sessions":nsess,"intra_threads":threads,"tracks":copies,"wall_s":wall,
            "tracks_per_hour":n/wall*3600.0,
            "mean_decode_ms":per.iter().map(|p|p.0).sum::<f64>()/n,
            "mean_clap_ms":per.iter().map(|p|p.1).sum::<f64>()/n,
            "mean_track_ms":per.iter().map(|p|p.2).sum::<f64>()/n,
            "aggregate_realtime_factor":dur*n/wall,"cuda":std::env::var("BENCH_CUDA").is_ok(),"vram_used_mib_at_end":vram})
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |i: usize| args[i].parse::<usize>().unwrap();
    match args[1].as_str() {
        "stages" => stages(Path::new(&args[2])),
        "rawdecode" => {
            let p = Path::new(&args[2]);
            let (t, f, r) = rawdecode(p);
            println!(
                "{}",
                json!({"mode":"rawdecode","profile":profile(),"file":p.file_name().unwrap().to_str(),"ms":t,"duration_s":f as f64/f64::from(r)})
            );
        }
        "micro" => micro(),
        "onnx" => onnx(num(2), num(3), num(4), num(5)),
        "pipe" => pipe(Path::new(&args[2]), num(3), num(4), num(5)),
        other => panic!("unknown mode {other}"),
    }
}
