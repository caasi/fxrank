//! Core scoring types and traits for fxrank.

pub mod confidence;
pub mod corpus;
pub mod effect;
pub mod frontend;
pub mod model;
pub mod score;

pub use corpus::{CorpusMatcher, CorpusProfile};
