//! Effect detectors for the Shell frontend.
//!
//! Each submodule classifies one effect family over the shared [`crate::walk::walk`] /
//! [`crate::walk::walk_commands`] descent (Task 2) — no detector re-implements the
//! traversal.

pub mod calls;
pub mod mutation;
