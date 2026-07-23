//! The Results section's heavier sub-widgets (task 20.1, split out of the
//! 1338-line `side_panel.rs`): the rep table, the velocity chart +
//! headline cards, and the metrics-education copy both read tooltips from.
//! `side_panel.rs` keeps only section orchestration and calls into these.

pub mod education;
pub mod rep_table;
pub mod velocity_chart;

pub use rep_table::rep_table;
pub use velocity_chart::{headline_card, velocity_chart};
