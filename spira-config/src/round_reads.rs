//! Every tree file the round machinery (round-vm, batcher-cut) opens by name. The suite corpus
//! is read from the branch's own tree, so a deleted suite is never asked for and is deliberately
//! absent.

pub const CONF_GEN: &str = "spira/conf-gen.sh";
pub const COVERAGE_ROUND: &str = "spira/coverage-round.sh";
pub const PLAN_MATRIX: &str = "spira/plan-matrix.sh";
pub const SANDBOX_RUNNER: &str = "tools/test-sandbox/runner";
pub const FIXTURE: &str = crate::retire::FIXTURE;

pub const TREE_READS: [&str; 5] = [CONF_GEN, COVERAGE_ROUND, PLAN_MATRIX, SANDBOX_RUNNER, FIXTURE];
