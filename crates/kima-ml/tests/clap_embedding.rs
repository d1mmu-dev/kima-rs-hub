use kima_ml::clap::{
    CLAP_EMBEDDING_DIM, ClapAggregationError, ClapEmbedding, MAX_CLAP_WINDOWS,
    aggregate_window_embeddings, mean_embeddings, normalize_embedding,
    postprocess_clap_model_outputs,
};

#[test]
fn test_window_count_bounds() {
    let empty: &[[f32; CLAP_EMBEDDING_DIM]] = &[];
    let err = aggregate_window_embeddings(empty).unwrap_err();
    assert_eq!(err, ClapAggregationError::EmptyWindows);

    let too_many = vec![[0.0_f32; CLAP_EMBEDDING_DIM]; MAX_CLAP_WINDOWS + 1];
    let err = aggregate_window_embeddings(&too_many).unwrap_err();
    assert_eq!(
        err,
        ClapAggregationError::TooManyWindows {
            count: MAX_CLAP_WINDOWS + 1,
            max: MAX_CLAP_WINDOWS,
        }
    );
}

#[test]
fn test_non_finite_inputs_are_rejected() {
    let mut nan_window = [0.0_f32; CLAP_EMBEDDING_DIM];
    nan_window[10] = f32::NAN;

    let mut inf_window = [0.0_f32; CLAP_EMBEDDING_DIM];
    inf_window[500] = f32::INFINITY;

    let mut neg_inf_window = [0.0_f32; CLAP_EMBEDDING_DIM];
    neg_inf_window[0] = f32::NEG_INFINITY;

    // normalize_embedding rejects non-finite
    assert_eq!(
        normalize_embedding(&nan_window).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
    assert_eq!(
        normalize_embedding(&inf_window).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
    assert_eq!(
        normalize_embedding(&neg_inf_window).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );

    // mean_embeddings rejects non-finite
    assert_eq!(
        mean_embeddings(&[nan_window]).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
    assert_eq!(
        mean_embeddings(&[[1.0; CLAP_EMBEDDING_DIM], inf_window]).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );

    // aggregate_window_embeddings rejects non-finite
    assert_eq!(
        aggregate_window_embeddings(&[nan_window]).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
    assert_eq!(
        aggregate_window_embeddings(&[[0.0; CLAP_EMBEDDING_DIM], neg_inf_window]).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
}

#[test]
fn test_zero_vector_and_cancellation() {
    let zero_window = [0.0_f32; CLAP_EMBEDDING_DIM];

    // Single zero window
    match normalize_embedding(&zero_window).unwrap_err() {
        ClapAggregationError::ZeroNorm { norm } => assert_eq!(norm, 0.0),
        other => panic!("expected ZeroNorm, got {other:?}"),
    }
    match aggregate_window_embeddings(&[zero_window]).unwrap_err() {
        ClapAggregationError::ZeroNorm { norm } => assert_eq!(norm, 0.0),
        other => panic!("expected ZeroNorm, got {other:?}"),
    }

    // Exact cancellation between two opposite valid unit vectors
    let mut w1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    w1[0] = 1.0;
    let mut w2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    w2[0] = -1.0;

    let mean = mean_embeddings(&[w1, w2]).unwrap();
    assert_eq!(mean, [0.0; CLAP_EMBEDDING_DIM]);

    match aggregate_window_embeddings(&[w1, w2]).unwrap_err() {
        ClapAggregationError::ZeroNorm { norm } => assert_eq!(norm, 0.0),
        other => panic!("expected ZeroNorm, got {other:?}"),
    }

    // Partial cancellation where non-cancelled dimensions survive
    let mut p1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    p1[0] = 1.0;
    p1[1] = 1.0;
    let mut p2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    p2[0] = -1.0;
    p2[1] = 1.0;

    let result = aggregate_window_embeddings(&[p1, p2]).unwrap();
    assert_eq!(result[0], 0.0);
    assert!((result[1] - 1.0).abs() < 1e-6);
    for i in 2..CLAP_EMBEDDING_DIM {
        assert_eq!(result[i], 0.0);
    }
}

#[test]
fn test_clap_embedding_unit_norm_guard() {
    // Exact unit vector
    let mut unit = [0.0_f32; CLAP_EMBEDDING_DIM];
    unit[0] = 1.0;
    let emb = ClapEmbedding::from_normalized(unit).unwrap();
    assert_eq!(emb.as_slice()[0], 1.0);
    assert_eq!(emb.as_slice().len(), CLAP_EMBEDDING_DIM);
    assert_eq!(emb.as_array()[0], 1.0);
    assert_eq!(emb.as_array(), &unit);
    assert_eq!(emb.into_inner()[0], 1.0);

    // Within tolerance 1e-3 (e.g. 1.0005)
    let mut close = [0.0_f32; CLAP_EMBEDDING_DIM];
    close[0] = 1.0005;
    assert!(ClapEmbedding::from_normalized(close).is_ok());

    // Deviating beyond 1e-3 (1.002)
    let mut high = [0.0_f32; CLAP_EMBEDDING_DIM];
    high[0] = 1.002;
    match ClapEmbedding::from_normalized(high).unwrap_err() {
        ClapAggregationError::UnitNormGuardFailed { norm } => {
            assert!((norm - 1.002).abs() < 1e-6);
        }
        other => panic!("expected UnitNormGuardFailed, got {other:?}"),
    }

    // Deviating below (0.998)
    let mut low = [0.0_f32; CLAP_EMBEDDING_DIM];
    low[0] = 0.998;
    match ClapEmbedding::from_normalized(low).unwrap_err() {
        ClapAggregationError::UnitNormGuardFailed { norm } => {
            assert!((norm - 0.998).abs() < 1e-6);
        }
        other => panic!("expected UnitNormGuardFailed, got {other:?}"),
    }

    // Non-finite
    let mut nan_vec = [0.0_f32; CLAP_EMBEDDING_DIM];
    nan_vec[0] = f32::NAN;
    assert_eq!(
        ClapEmbedding::from_normalized(nan_vec).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );
}

#[test]
fn test_manually_calculated_known_vectors() {
    // 1 window: [3, 4, 0, ...] -> norm 5 -> normalized [0.6, 0.8, 0, ...]
    let mut w1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    w1[0] = 3.0;
    w1[1] = 4.0;
    let res1 = aggregate_window_embeddings(&[w1]).unwrap();
    assert!((res1[0] - 0.6).abs() < 1e-6);
    assert!((res1[1] - 0.8).abs() < 1e-6);
    for i in 2..CLAP_EMBEDDING_DIM {
        assert_eq!(res1[i], 0.0);
    }

    // 2 windows: orthogonal unit basis e0 and e1
    // Mean: [0.5, 0.5, 0, ...]
    // L2 norm of mean: sqrt(0.5^2 + 0.5^2) = sqrt(0.5) = 1/sqrt(2)
    // Normalized: [1/sqrt(2), 1/sqrt(2), 0, ...]
    let mut e0 = [0.0_f32; CLAP_EMBEDDING_DIM];
    e0[0] = 1.0;
    let mut e1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    e1[1] = 1.0;
    let res2 = aggregate_window_embeddings(&[e0, e1]).unwrap();
    let inv_sqrt_2 = 1.0_f32 / 2.0_f32.sqrt();
    assert!((res2[0] - inv_sqrt_2).abs() < 1e-6);
    assert!((res2[1] - inv_sqrt_2).abs() < 1e-6);
    for i in 2..CLAP_EMBEDDING_DIM {
        assert_eq!(res2[i], 0.0);
    }

    // 3 windows: e0, e1, e2
    // Mean: [1/3, 1/3, 1/3, 0, ...]
    // Normalized: [1/sqrt(3), 1/sqrt(3), 1/sqrt(3), 0, ...]
    let mut e2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    e2[2] = 1.0;
    let res3 = aggregate_window_embeddings(&[e0, e1, e2]).unwrap();
    let inv_sqrt_3 = 1.0_f32 / 3.0_f32.sqrt();
    assert!((res3[0] - inv_sqrt_3).abs() < 1e-6);
    assert!((res3[1] - inv_sqrt_3).abs() < 1e-6);
    assert!((res3[2] - inv_sqrt_3).abs() < 1e-6);
    for i in 3..CLAP_EMBEDDING_DIM {
        assert_eq!(res3[i], 0.0);
    }

    // 4 windows: e0, e1, e2, e3
    // Mean: [0.25, 0.25, 0.25, 0.25, 0, ...]
    // Norm of mean: sqrt(4 * 0.0625) = 0.5
    // Normalized: [0.5, 0.5, 0.5, 0.5, 0, ...]
    let mut e3 = [0.0_f32; CLAP_EMBEDDING_DIM];
    e3[3] = 1.0;
    let res4 = aggregate_window_embeddings(&[e0, e1, e2, e3]).unwrap();
    for i in 0..4 {
        assert!((res4[i] - 0.5).abs() < 1e-6);
    }
    for i in 4..CLAP_EMBEDDING_DIM {
        assert_eq!(res4[i], 0.0);
    }

    // 5 windows: e0, e1, e2, e3, e4
    // Mean: [0.2, 0.2, 0.2, 0.2, 0.2, 0, ...]
    // Norm of mean: sqrt(5 * 0.04) = sqrt(0.2) = 1/sqrt(5)
    // Normalized: [1/sqrt(5), 1/sqrt(5), 1/sqrt(5), 1/sqrt(5), 1/sqrt(5), 0, ...]
    let mut e4 = [0.0_f32; CLAP_EMBEDDING_DIM];
    e4[4] = 1.0;
    let res5 = aggregate_window_embeddings(&[e0, e1, e2, e3, e4]).unwrap();
    let inv_sqrt_5 = 1.0_f32 / 5.0_f32.sqrt();
    for i in 0..5 {
        assert!((res5[i] - inv_sqrt_5).abs() < 1e-6);
    }
    for i in 5..CLAP_EMBEDDING_DIM {
        assert_eq!(res5[i], 0.0);
    }
}

#[test]
fn test_postprocess_clap_model_outputs_per_window_normalization() {
    // Upstream semantics: raw model outputs have different magnitudes across windows.
    // embed_clips L2-normalizes each window FIRST before computing the mean.
    // Raw window 1: [30.0, 40.0, 0, ...] -> norm 50 -> unit-norm: [0.6, 0.8, 0, ...]
    // Raw window 2: [0.0, 0.0, 100.0, 0, ...] -> norm 100 -> unit-norm: [0, 0, 1.0, 0, ...]
    // Mean of unit windows: [0.3, 0.4, 0.5, 0, ...]
    // Norm of mean: sqrt(0.09 + 0.16 + 0.25) = sqrt(0.5) = 1/sqrt(2)
    // Final normalized: [0.3 * sqrt(2), 0.4 * sqrt(2), 0.5 * sqrt(2), 0, ...]
    let mut raw1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    raw1[0] = 30.0;
    raw1[1] = 40.0;

    let mut raw2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    raw2[2] = 100.0;

    let res = postprocess_clap_model_outputs(&[raw1, raw2]).unwrap();
    let sqrt_2 = 2.0_f32.sqrt();
    let expected_0 = 0.3 * sqrt_2;
    let expected_1 = 0.4 * sqrt_2;
    let expected_2 = 0.5 * sqrt_2;

    assert!((res[0] - expected_0).abs() < 1e-6);
    assert!((res[1] - expected_1).abs() < 1e-6);
    assert!((res[2] - expected_2).abs() < 1e-6);
    for i in 3..CLAP_EMBEDDING_DIM {
        assert_eq!(res[i], 0.0);
    }

    // Verify unit norm of result
    let norm = (res[0] * res[0] + res[1] * res[1] + res[2] * res[2]).sqrt();
    assert!((norm - 1.0).abs() < 1e-6);
}

#[test]
fn test_postprocess_clap_model_outputs_errors_and_idempotence() {
    // Empty
    assert_eq!(
        postprocess_clap_model_outputs(&[]).unwrap_err(),
        ClapAggregationError::EmptyWindows
    );

    // Too many
    let too_many = vec![[1.0_f32; CLAP_EMBEDDING_DIM]; 6];
    assert_eq!(
        postprocess_clap_model_outputs(&too_many).unwrap_err(),
        ClapAggregationError::TooManyWindows { count: 6, max: 5 }
    );

    // Non-finite
    let mut nan_window = [1.0_f32; CLAP_EMBEDDING_DIM];
    nan_window[42] = f32::NAN;
    assert_eq!(
        postprocess_clap_model_outputs(&[nan_window]).unwrap_err(),
        ClapAggregationError::NonFiniteInput
    );

    // Zero window
    let zero_window = [0.0_f32; CLAP_EMBEDDING_DIM];
    match postprocess_clap_model_outputs(&[zero_window]).unwrap_err() {
        ClapAggregationError::ZeroNorm { norm } => assert_eq!(norm, 0.0),
        other => panic!("expected ZeroNorm, got {other:?}"),
    }

    // Cancellation
    let mut w1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    w1[0] = 5.0; // raw vector
    let mut w2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    w2[0] = -10.0; // opposite raw vector: normalizes to [-1, 0, ...]
    match postprocess_clap_model_outputs(&[w1, w2]).unwrap_err() {
        ClapAggregationError::ZeroNorm { norm } => assert_eq!(norm, 0.0),
        other => panic!("expected ZeroNorm, got {other:?}"),
    }

    // Idempotence: for already-normalized inputs, postprocess_clap_model_outputs
    // matches aggregate_window_embeddings exactly.
    let mut u1 = [0.0_f32; CLAP_EMBEDDING_DIM];
    u1[0] = 1.0;
    let mut u2 = [0.0_f32; CLAP_EMBEDDING_DIM];
    u2[1] = 1.0;

    let res_agg = aggregate_window_embeddings(&[u1, u2]).unwrap();
    let res_post = postprocess_clap_model_outputs(&[u1, u2]).unwrap();
    assert_eq!(res_agg, res_post);
}
