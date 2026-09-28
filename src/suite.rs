//! The suites, and the target each one is measured against.
//!
//! The list mirrors `spec/13_observability_testing_bench.md` section 3 and the targets come from
//! `spec/02_requirements_slos.md`. A suite is listed here before it can run, so that `list` shows
//! what is missing as well as what exists.

/// Where a suite runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One bare metal node.
    Node,
    /// A scale unit, or as much of one as there is hardware for.
    Cluster,
    /// A real dataset, end to end through a cluster.
    Dataset,
}

/// One benchmark suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suite {
    /// The name used on the command line and in report paths.
    pub name: &'static str,
    /// Where it runs.
    pub scope: Scope,
    /// What it measures.
    pub measures: &'static str,
    /// The target it is judged against.
    pub target: &'static str,
    /// The hivebox milestone at which it can first run.
    pub milestone: &'static str,
}

/// Every suite, in the order a report prints them.
pub const SUITES: &[Suite] = &[
    Suite {
        name: "create-storm",
        scope: Scope::Node,
        measures: "creates a second sustained until p99 breaks, per backend",
        target: "container 300/s or more, microVM 150/s or more",
        milestone: "M0",
    },
    Suite {
        name: "exec",
        scope: Scope::Node,
        measures: "no-op exec round trip and throughput at 2,500 cells",
        target: "p50 5 ms or less, 50K/s or more",
        milestone: "M0",
    },
    Suite {
        name: "cold-image",
        scope: Scope::Node,
        measures: "first create from an uncached 10 GiB image, with and without trace prefetch",
        target: "p99 5 s or less",
        milestone: "M1",
    },
    Suite {
        name: "snapshot",
        scope: Scope::Node,
        measures: "pause and resume, disk snapshot at 10, 100 and 1000 MiB dirty, fork of 1 to 16",
        target: "fork of 8 in 300 ms or less",
        milestone: "M2",
    },
    Suite {
        name: "density",
        scope: Scope::Node,
        measures: "most cells at p99 exec of 25 ms or less with the DSec CPU distribution",
        target: "3,200 containers or 800 microVMs",
        milestone: "M2",
    },
    Suite {
        name: "memory",
        scope: Scope::Node,
        measures: "peak and time integrated memory with and without pmem-DAX, FPR and DAMON",
        target: "reproduce DSec's 40.2% and 21.2% reductions",
        milestone: "M2",
    },
    Suite {
        name: "cpu-qos",
        scope: Scope::Node,
        measures: "latency inflation of latency class cells under best effort saturation",
        target: "20% or less",
        milestone: "M2",
    },
    Suite {
        name: "replay",
        scope: Scope::Cluster,
        measures: "synthetic workload fit to the DSec lifetime, burst and image distributions",
        target: "keeper write rate flat as load grows",
        milestone: "M1",
    },
    Suite {
        name: "headline",
        scope: Scope::Cluster,
        measures: "creates a second sustained for 30 minutes with 400K concurrent",
        target: "5,000/s, infra errors 0.1% or less",
        milestone: "M2",
    },
    Suite {
        name: "node-loss",
        scope: Scope::Cluster,
        measures: "5% of nodes killed mid burst",
        target: "every lost cell classified and masked",
        milestone: "M1",
    },
    Suite {
        name: "swe-bench-verified",
        scope: Scope::Dataset,
        measures: "gold patch passes and empty patch fails on all 500 tasks",
        target: "99% or more on the container tier",
        milestone: "M0",
    },
    Suite {
        name: "image-import",
        scope: Scope::Dataset,
        measures: "dedup ratio and storage for SWE-Gym, R2E-Gym, SWE-smith and friends",
        target: "reported, no fixed target",
        milestone: "M1",
    },
    Suite {
        name: "grpo",
        scope: Scope::Dataset,
        measures: "verl or slime GRPO on SWE-Gym against a Docker and Kubernetes baseline",
        target: "infra masked 0.1% or less over a week",
        milestone: "M3",
    },
];

/// The suite with this name.
#[must_use]
pub fn find(name: &str) -> Option<&'static Suite> {
    SUITES.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_findable() {
        for (i, s) in SUITES.iter().enumerate() {
            assert_eq!(find(s.name), Some(s));
            assert!(SUITES[i + 1..].iter().all(|o| o.name != s.name), "{} twice", s.name);
        }
        assert_eq!(find("nope"), None);
    }

    #[test]
    fn every_suite_names_a_real_milestone() {
        for s in SUITES {
            assert!(["M0", "M1", "M2", "M3"].contains(&s.milestone), "{}", s.name);
        }
    }
}
