//! The music CLAP text input contract (kima-hub c7527577 `music_clap_onnx.MusicClapText.tokens`):
//! `RobertaTokenizer('roberta-base')(texts, padding='max_length', truncation=True,
//! max_length=77)`, so every prompt is exactly 77 ids, padded with `<pad>` (1), and the
//! attention mask marks the real tokens.
use kima_ml::text_tokenizer::{CLAP_TEXT_MAX_TOKENS, CLAP_TEXT_PAD_ID, pad_to_max_length};

#[test]
fn short_prompts_are_padded_to_77_with_a_matching_mask() {
    let (ids, mask) = pad_to_max_length(&[0, 42891, 232, 2]).unwrap();
    assert_eq!(ids.len(), CLAP_TEXT_MAX_TOKENS);
    assert_eq!(mask.len(), CLAP_TEXT_MAX_TOKENS);
    assert_eq!(&ids[..4], &[0, 42891, 232, 2]);
    assert!(ids[4..].iter().all(|&id| id == CLAP_TEXT_PAD_ID));
    assert_eq!(&mask[..4], &[1, 1, 1, 1]);
    assert!(mask[4..].iter().all(|&m| m == 0));
}

#[test]
fn a_full_length_prompt_is_unpadded_and_fully_attended() {
    let mut full = vec![5_i64; CLAP_TEXT_MAX_TOKENS];
    full[0] = 0;
    full[CLAP_TEXT_MAX_TOKENS - 1] = 2;
    let (ids, mask) = pad_to_max_length(&full).unwrap();
    assert_eq!(ids, full);
    assert!(mask.iter().all(|&m| m == 1));
}

#[test]
fn empty_and_over_length_encodings_are_refused() {
    assert!(pad_to_max_length(&[]).is_err());
    assert!(pad_to_max_length(&vec![5; CLAP_TEXT_MAX_TOKENS + 1]).is_err());
}
