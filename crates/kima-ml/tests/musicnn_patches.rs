use kima_ml::musicnn::mel_patches;

#[test]
fn patches_preserve_frame_and_band_order_at_the_specified_stride() {
    let frames: Vec<[f32; 96]> = (0..374)
        .map(|frame| std::array::from_fn(|band| (frame * 100 + band) as f32))
        .collect();
    let patches: Vec<_> = mel_patches(&frames).collect();
    assert_eq!(patches.len(), 3);
    for (patch, start) in patches.iter().zip([0, 93, 186]) {
        assert_eq!(patch.len(), 187);
        for (frame, row) in patch.iter().enumerate() {
            for (band, value) in row.iter().enumerate() {
                assert_eq!(*value, ((start + frame) * 100 + band) as f32);
            }
        }
    }
}

#[test]
fn patch_counts_cover_short_exact_and_partial_boundaries() {
    for (length, expected) in [
        (0, 0),
        (1, 0),
        (186, 0),
        (187, 1),
        (279, 1),
        (280, 2),
        (372, 2),
        (373, 3),
    ] {
        let frames = vec![[0.0; 96]; length];
        assert_eq!(mel_patches(&frames).count(), expected, "length {length}");
    }
}
