//! The day's mixes (upstream `services/mixes/index.ts` `generateAllMixes`): the 42 generators in upstream's order, the
//! five a day plays (picked by the LCG over the day's seed), and the replacement of a mix that failed or came back
//! empty by the next generator not yet tried.

use std::collections::HashSet;
use std::future::Future;

use crate::mix::ProgrammaticMix;

/// Upstream `DAILY_MIX_COUNT`.
pub const DAILY_MIX_COUNT: usize = 5;

/// Upstream's generators, in `mixGenerators` order (the LCG indexes into this list).
pub const GENERATORS: [&str; 42] = [
    "Era Mix",
    "Genre Mix",
    "Top Tracks Mix",
    "Rediscover Mix",
    "Artist Similar Mix",
    "Random Discovery Mix",
    "Party Mix",
    "Chill Mix",
    "Workout Mix",
    "Focus Mix",
    "High Energy Mix",
    "Late Night Mix",
    "Happy Vibes Mix",
    "Melancholy Mix",
    "Dance Floor Mix",
    "Acoustic Mix",
    "Instrumental Mix",
    "Road Trip Mix",
    "Day Mix",
    "Sad Girl Sundays",
    "Main Character Energy",
    "Villain Era",
    "Hot Girl Walk",
    "Rage Cleaning",
    "Golden Hour",
    "Shower Karaoke",
    "In My Feelings",
    "Midnight Drive",
    "Coffee Shop Vibes",
    "Romanticize Your Life",
    "That Girl Era",
    "Unhinged",
    "Sleep",
    "Summer",
    "Epic",
    "Dreamy",
    "Nostalgic",
    "Deep Cuts",
    "Key Journey",
    "Tempo Flow",
    "Vocal Detox",
    "Minor Key Mondays",
];

/// Upstream's selection and fill: the `selected` generators (each `None` when it failed or had no mix), then, while
/// fewer than [`DAILY_MIX_COUNT`] came back, every generator not yet tried, in order, whose mix is of a type not yet
/// in the list.
pub async fn assemble<F, Fut>(selected: &[usize], mut generate: F) -> Vec<ProgrammaticMix>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Option<ProgrammaticMix>>,
{
    let mut mixes: Vec<ProgrammaticMix> = Vec::new();
    for &i in selected {
        if let Some(mix) = generate(i).await {
            mixes.push(mix);
        }
    }
    if mixes.len() < DAILY_MIX_COUNT {
        let mut types: HashSet<String> = mixes.iter().map(|m| m.kind.clone()).collect();
        for i in 0..GENERATORS.len() {
            if mixes.len() >= DAILY_MIX_COUNT {
                break;
            }
            if selected.contains(&i) {
                continue;
            }
            if let Some(mix) = generate(i).await
                && !types.contains(&mix.kind)
            {
                types.insert(mix.kind.clone());
                mixes.push(mix);
            }
        }
    }
    mixes
}
