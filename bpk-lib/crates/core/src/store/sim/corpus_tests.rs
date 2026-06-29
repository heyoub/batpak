//! Unit tests for [`super`] (split to keep `corpus.rs` under the file-size cap).

use super::super::recovery::run;
use super::{check_graduation, run_corpus_sweep, Boundary, FaultModeLabel, GraduationRefusal};

#[test]
fn fault_mode_label_round_trips_through_serialized_form() {
    for (label, expect) in [
        (FaultModeLabel::HonestDiskCrash, "HonestDiskCrash"),
        (
            FaultModeLabel::LyingDiskFsyncDrop { one_in: 3 },
            "LyingDiskFsyncDrop",
        ),
        (FaultModeLabel::CrashBeforeFsync, "CrashBeforeFsync"),
    ] {
        assert_eq!(label.as_str(), expect, "label must serialize stably");
        let parsed = FaultModeLabel::parse(label.as_str()).expect("label must re-parse");
        assert_eq!(
            parsed.as_str(),
            label.as_str(),
            "parse∘as_str must be identity on the label"
        );
    }
    assert!(
        FaultModeLabel::parse("NotARealMode").is_none(),
        "unknown labels must not parse"
    );
    assert_eq!(
        FaultModeLabel::parse_with_rate("LyingDiskFsyncDrop", Some(0)),
        Some(FaultModeLabel::LyingDiskFsyncDrop { one_in: 1 }),
        "a zero drop-rate must clamp to the legal floor of 1"
    );
    assert!(
        FaultModeLabel::parse_with_rate("LyingDiskFsyncDrop", None).is_none(),
        "a lying-disk row without a drop-rate column must be rejected"
    );
}

#[test]
fn boundary_round_trips_through_serialized_form() {
    for boundary in Boundary::ALL {
        let parsed = Boundary::parse(boundary.as_str()).expect("boundary must re-parse");
        assert_eq!(
            parsed, boundary,
            "parse∘as_str must be identity on every boundary"
        );
    }
    assert!(
        Boundary::parse("NotABoundary").is_none(),
        "unknown boundary labels must not parse"
    );
}

#[test]
fn graduation_refuses_nondeterministic_seed() -> Result<(), Box<dyn std::error::Error>> {
    let seed = 0xC000_0001;
    let steps = 48;
    let first = run(seed, steps).map_err(std::io::Error::other)?;
    let mismatched = run(seed, steps + 1).map_err(std::io::Error::other)?;
    if first.digest == mismatched.digest {
        return Err(std::io::Error::other(
            "PROPERTY: distinct step counts should diverge for this fixture",
        )
        .into());
    }
    let refusal = GraduationRefusal::NonDeterministic {
        seed,
        first: first.digest,
        second: mismatched.digest,
    };
    assert!(
        refusal.to_string().contains("non-deterministic"),
        "refusal must name non-determinism: {refusal}"
    );
    Ok(())
}

#[test]
fn graduation_accepts_deterministic_legal_seed() -> Result<(), Box<dyn std::error::Error>> {
    let candidate = check_graduation(0xC000_0002, 64, "writer-commit", "L4")
        .map_err(|r| std::io::Error::other(format!("PROPERTY: legal seed must graduate: {r}")))?;
    assert_eq!(candidate.entry.seam_touched, "writer-commit");
    assert_eq!(candidate.entry.assurance_level, "L4");
    let again = check_graduation(0xC000_0002, 64, "writer-commit", "L4")
        .map_err(|r| std::io::Error::other(format!("PROPERTY: replay must re-graduate: {r}")))?;
    assert_eq!(
        candidate.entry.op_trace_digest, again.entry.op_trace_digest,
        "PROPERTY: digest must be stable across graduation calls"
    );
    Ok(())
}

#[test]
fn committed_corpus_seed_digest_is_stable() -> Result<(), Box<dyn std::error::Error>> {
    let candidate = check_graduation(48104590831, 96, "writer-commit", "L4").map_err(|r| {
        std::io::Error::other(format!(
            "PROPERTY: committed corpus seed must graduate: {r}"
        ))
    })?;
    assert_eq!(
        candidate.entry.op_trace_digest, 101_395_256_710_529_115,
        "PROPERTY: committed corpus digest for seed 48104590831 / 96 steps must be stable"
    );
    Ok(())
}

#[test]
fn sweep_emits_candidates_for_legal_seeds() -> Result<(), Box<dyn std::error::Error>> {
    let (ok, bad) = run_corpus_sweep(&[0xC000_0003, 0xC000_0004], 48, "writer-commit", "L4");
    if ok.len() != 2 {
        return Err(std::io::Error::other(format!(
            "PROPERTY: expected two graduates, got {} ok and {} refused",
            ok.len(),
            bad.len()
        ))
        .into());
    }
    Ok(())
}

#[test]
fn empty_seam_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    match check_graduation(0xC000_0005, 32, "", "L4") {
        Ok(_) => {
            return Err(
                std::io::Error::other("PROPERTY: empty seam_touched must be refused").into(),
            )
        }
        Err(GraduationRefusal::EmptySeam { .. }) => {}
        Err(other) => {
            return Err(std::io::Error::other(format!(
                "PROPERTY: expected EmptySeam refusal, got {other}"
            ))
            .into())
        }
    }
    Ok(())
}
