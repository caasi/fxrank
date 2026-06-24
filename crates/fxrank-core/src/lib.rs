//! Core scoring types and traits for fxrank.

pub mod confidence;
pub mod corpus;
pub mod effect;
pub mod fold;
pub mod frontend;
pub mod graph;
pub mod model;
pub mod record;
pub mod resolve;
pub mod score;

pub use corpus::{CorpusMatcher, CorpusProfile};
