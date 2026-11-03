use kima_ml::musicnn::{MusicnnFrontend, mel_patches};

#[test]
fn rejects_nonfinite_and_excessive_input_and_overflow() {
    let mut frontend = MusicnnFrontend::new();
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
        assert!(frontend.compute(&[sample]).is_err());
    }
    assert!(frontend.compute(&vec![0.0; 1_440_001]).is_err());
    assert!(frontend.compute(&[]).unwrap().is_empty());
}

#[test]
fn generated_silent_frames_feed_existing_patch_slices() {
    let mut frontend = MusicnnFrontend::new();
    let frames = frontend.compute(&vec![0.0; 186 * 256]).unwrap();
    assert_eq!(frames.len(), 187);
    assert!(frames.iter().flatten().all(|&band| band == 0.0));
    let patches: Vec<_> = mel_patches(&frames).collect();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0], &frames[..]);
}
