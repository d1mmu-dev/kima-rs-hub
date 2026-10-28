//! The autotagger's data (spec §6.8, §19): per-album search state, stored candidates, the
//! local album as the matcher sees it and the values an apply writes.

pub mod albums;
pub mod applied;

pub use albums::*;
pub use applied::*;
