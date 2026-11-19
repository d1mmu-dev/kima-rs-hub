//! The day's rotation (M5b Task 9; port of upstream `services/mixes/__tests__/mixRotationFailures.test.ts` at the pin
//! `c7527577`): a selected generator that fails or has no mix is replaced by the next generator not yet tried, and a
//! replacement of a type already in the list is skipped.

use std::cell::RefCell;

use kima_mixes::daily::{DAILY_MIX_COUNT, GENERATORS, assemble};
use kima_mixes::mix::{MixSpec, ProgrammaticMix, mix_of};
use kima_mixes::seeded::{daily_pick, seeded_random};

fn mix(i: usize, kind: &str) -> ProgrammaticMix {
    let spec = MixSpec {
        kind,
        name: GENERATORS[i],
        description: "",
        color: kind,
    };
    mix_of(format!("m{i}"), spec, &[])
}

#[tokio::test]
async fn a_failed_generator_is_replaced_by_the_next_untried_type() {
    assert_eq!(GENERATORS.len(), 42);
    let selected = [3, 7, 11, 20, 30];
    let tried = RefCell::new(Vec::new());
    // 7 and 20 fail; 0 is tried first among the rest and is of a type already listed (3's), so 1 and 2 fill in.
    let got = assemble(&selected, |i| {
        tried.borrow_mut().push(i);
        async move {
            match i {
                7 | 20 => None,
                0 | 3 => Some(mix(i, "same")),
                _ => Some(mix(i, &format!("t{i}"))),
            }
        }
    })
    .await;
    let ids: Vec<String> = got.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, ["m3", "m11", "m30", "m1", "m2"]);
    assert_eq!(*tried.borrow(), [3, 7, 11, 20, 30, 0, 1, 2]);
}

#[tokio::test]
async fn every_generator_failing_gives_an_empty_list_not_an_error() {
    let selected = daily_pick(
        seeded_random("2026-11-19-someone"),
        GENERATORS.len(),
        DAILY_MIX_COUNT,
    );
    let got = assemble(&selected, |_| async { None }).await;
    assert!(got.is_empty());
}

#[tokio::test]
async fn five_selected_mixes_need_no_fill() {
    let selected = [0, 1, 2, 3, 4];
    let tried = RefCell::new(0);
    let got = assemble(&selected, |i| {
        *tried.borrow_mut() += 1;
        async move { Some(mix(i, &format!("t{i}"))) }
    })
    .await;
    assert_eq!(got.len(), 5);
    assert_eq!(*tried.borrow(), 5);
}
