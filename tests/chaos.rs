#![cfg(target_os = "linux")]
#![cfg(feature = "dangerous-test-hooks")]
// justifies: INV-TEST-PANIC-AS-ASSERTION, INV-CHAOS-LINUX-ONLY; tests/chaos.rs is a Linux-only privileged harness whose assertions use panic/expect and whose cleanup path may print best-effort teardown warnings.
#![allow(clippy::panic, clippy::print_stderr, clippy::unwrap_used)]

#[path = "chaos/mod.rs"]
mod chaos;
