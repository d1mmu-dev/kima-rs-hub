//! M5b mixes, radio and what's-new storage (migration 0063): the saved mood mix
//! (`user_mood_mix`), saved mix playlists (`playlists.mix_id`), release acknowledgements and
//! choices, and `tracks.mood_tags`. Every personal row is keyed by the profile (M-PROF).
//! Queries arrive with the tasks that use them (M5b plan Tasks 4, 9, 10, 15, 16).

pub mod daily;
pub mod familiar;
pub mod generators;
pub mod mood_mix;
pub mod night;
pub mod recipe;
pub mod recipe_mixes;
pub mod save;
pub mod tags;
pub mod track_rows;
