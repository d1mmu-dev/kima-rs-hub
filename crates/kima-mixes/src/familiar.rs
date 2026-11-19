//! The familiarity boost's shape (upstream `services/mixes/familiarity.ts` `FamiliarBoost`); the scan that computes it
//! is M5b Task 8.

use uuid::Uuid;

/// Song ids and their boosts, 0 to 1, in matching order, as parallel lists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FamiliarBoost {
    pub ids: Vec<Uuid>,
    pub boosts: Vec<f64>,
}
