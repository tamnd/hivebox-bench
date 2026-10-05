# Changelog

Every change that a person reading a report would care about goes here, newest first. A change to how a number is computed is always one of those.

## Unreleased

- The first report, `reports/2026-10-04`: image-import, cold-image, replay and node-loss on one shared VM.
- The M1 suites: replay and node-loss through a gate, with the keeper's write rate read from its applied index, and cold-image and image-import through the `hive-nectar` binary. hive-sdk moves to v0.0.17, and hive-proto at the same tag is new.
- The suite list, with the target each suite is judged against and the hivebox milestone it needs.
- Nearest rank percentiles and the summary every report prints.
- CI that checks the harness builds and works, and never times anything.
