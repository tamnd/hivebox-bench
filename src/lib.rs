//! The benchmark harness for hivebox.
//!
//! The suites are listed in [`suite`] and the arithmetic every report shares is in [`stats`]. The
//! node suites that drive one comb are in [`node`] and [`qos`], the cluster suites that go through a gate are
//! in [`cluster`], and the image suites that drive `hive-nectar` are in [`image`]. The design is
//! `spec/13_observability_testing_bench.md` section 3 in the hivebox repository.

#![forbid(unsafe_code)]

pub mod cluster;
pub mod image;
pub mod node;
pub mod qos;
pub mod raw;
pub mod stats;
pub mod suite;
