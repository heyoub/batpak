use crate::util::run;
use crate::{ChaosArgs, FuzzArgs};
use anyhow::Result;
use std::process::Command;

pub(crate) fn fuzz(args: FuzzArgs) -> Result<()> {
    let cases = if args.deep { "100000" } else { "10000" };
    let mut command = Command::new("cargo");
    command.env("PROPTEST_CASES", cases);
    command.args([
        "test",
        "--test",
        "fuzz_targets",
        "--all-features",
        "--release",
        "--",
        "--nocapture",
    ]);
    run(command)
}

pub(crate) fn chaos(args: ChaosArgs) -> Result<()> {
    let iterations = if args.deep { "5000" } else { "2000" };
    let mut command = Command::new("cargo");
    command.env("CHAOS_ITERATIONS", iterations);
    command.args([
        "test",
        "--test",
        "chaos_testing",
        "--all-features",
        "--release",
        "--",
        "--nocapture",
    ]);
    run(command)
}
