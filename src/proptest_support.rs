//! One deterministic harness for every property test in the tree.
//!
//! ## Why determinism is not optional here
//!
//! proptest's default runner seeds from entropy, so each run explores different
//! inputs. Three consequences, and the third is the one that forced this:
//!
//! 1. **Flaky CI.** A property can pass today and fail tomorrow with no code
//!    change. The failure is real, but it arrives detached from the commit that
//!    introduced it, which is the worst possible time to learn about it.
//! 2. **Unreproducible failures.** "It failed on CI" is not actionable if the
//!    inputs differ locally.
//! 3. **Unstable coverage.** `#436`–`#438` armed a per-file coverage ratchet
//!    with hard floors. Different explored inputs execute different branches, so
//!    entropy-seeded properties make per-file coverage wander, and the ratchet
//!    fails on PRs that changed nothing. A gate that cries wolf gets bypassed,
//!    and we have already spent a night on one gate that was quietly doing
//!    nothing — a noisy one is the same failure with more steps.
//!
//! ## The trade-off, stated
//!
//! A fixed seed means the property checks the same inputs forever: it stops
//! being a search and becomes a large, precisely-chosen test suite. That is the
//! right default for a gate, but it does give up proptest's ability to find new
//! counterexamples over time.
//!
//! [`explore`] is the escape hatch. `PROPTEST_EXPLORE=1 cargo test` seeds from
//! entropy and re-enables failure persistence, so a deliberate hunt still works
//! and anything it finds is written to `proptest-regressions/` to be committed
//! as a permanent case. Off by default, so CI and coverage stay stable.

use proptest::test_runner::{Config, FileFailurePersistence, TestRng, TestRunner};

/// True when the caller asked for a randomised hunt rather than the gate.
fn explore() -> bool {
    std::env::var("PROPTEST_EXPLORE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// A runner with a fixed seed, so a given `cases` count explores exactly the
/// same inputs on every machine and every run.
///
/// Failure persistence is off in the default (deterministic) mode on purpose:
/// with a fixed seed a failing case recurs every run anyway, so a regression
/// file would add an untracked artifact that pins nothing new. In explore mode
/// it is switched back on, because there the discovery genuinely is unrepeatable
/// and worth committing.
pub fn runner(cases: u32) -> TestRunner {
    if explore() {
        return TestRunner::new(Config {
            cases,
            failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
                "proptest-regressions",
            ))),
            ..Config::default()
        });
    }
    // `TestRunner::deterministic()` hardcodes `Config::default()`, so build the
    // same thing directly to carry our own `cases` and persistence setting.
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    let algorithm = config.rng_algorithm;
    TestRunner::new_with_rng(config, TestRng::deterministic_rng(algorithm))
}

/// Run `body` over `strategy`, panicking with proptest's shrunk counterexample.
///
/// Wrapping `TestRunner::run` keeps shrinking intact — a failure still minimises
/// to the smallest input that breaks the property, which is most of proptest's
/// value and the reason this is not simply a loop over a fixed input table.
pub fn check<S, F>(cases: u32, strategy: S, body: F)
where
    S: proptest::strategy::Strategy,
    F: Fn(S::Value) -> Result<(), proptest::test_runner::TestCaseError>,
{
    if let Err(e) = runner(cases).run(&strategy, body) {
        panic!("{e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The whole point: two runners built the same way must agree, input for
    /// input. If this ever fails, every property in the tree is a coin flip and
    /// the coverage ratchet's floors are noise.
    #[test]
    fn two_runs_explore_identical_inputs() {
        let collect = || {
            let seen = std::cell::RefCell::new(Vec::new());
            let _ = runner(24).run(&(0u32..1_000_000u32), |v| {
                seen.borrow_mut().push(v);
                Ok(())
            });
            seen.into_inner()
        };
        let a = collect();
        let b = collect();
        assert!(!a.is_empty(), "runner produced no cases");
        assert_eq!(a, b, "the deterministic runner is not deterministic");
    }

    /// A failing property must still shrink, or determinism has been bought by
    /// turning proptest into a fixed loop.
    #[test]
    fn failures_still_shrink() {
        let err = runner(64)
            .run(&(0u32..10_000u32), |v| {
                prop_assert!(v < 500, "boom");
                Ok(())
            })
            .expect_err("must fail");
        assert!(
            format!("{err}").contains("500"),
            "expected a shrunk counterexample near the 500 boundary, got: {err}"
        );
    }
}
