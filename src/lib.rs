//! The benchmark harness for hivebox.
//!
//! The suites are listed in [`suite`] and the arithmetic every report shares is in [`stats`]. The
//! load generator, trace replay and the real data runs arrive with the milestones, and the design
//! is `spec/13_observability_testing_bench.md` section 3 in the hivebox repository.

#![forbid(unsafe_code)]

pub mod stats;
pub mod suite;
