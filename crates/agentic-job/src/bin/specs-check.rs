//! specs-check: model-checks the TLA+ specifications in `specs/`, each
//! good configuration and each deliberately broken one.
//!
//! ```text
//! specs-check SPECS_DIR
//! ```
//!
//! It downloads TLC's `tla2tools.jar` into a new temporary directory and
//! refuses to execute it unless its SHA-256 is the one pinned here. A good
//! configuration must exit 0 with TLC's completion message; a broken one
//! must exit 12 naming the invariant it breaks, so that a parse error, a
//! Java failure or an unexpected success fails the check rather than
//! passing for a found violation. `JAVA_BIN` names a non-default Java.
//! Full reports stay in the temporary directory, printed first.

use anyhow::{Context, Result, bail, ensure};
use std::path::Path;
use std::process::Command;

// Stable v1.7.4, not the v1.8.0 prerelease whose jar upstream re-uploads. The
// v1.7.4 release notes publish SHA-1 bee4a54f3ee3d4afc347c3240ec2d9e93b075104;
// this is the SHA-256 of the download matching it, pinned in source and never
// fetched beside the executable. TLC 1.7.4 writes no SpecTE trace module unless
// asked to (-generateSpecTE), so the checkout stays clean.
const JAR_URL: &str = "https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar";
const JAR_SHA256: &str = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88";
const JAR_NAME: &str = "tla2tools.jar";
const JAVA_BIN_ENV: &str = "JAVA_BIN";
/// TLC's exit status for a violated invariant.
const TLC_VIOLATION: i32 = 12;
const COMPLETED: &str = "Model checking completed. No error";
/// What one TLC run may print; more fails the check.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// (module, configuration, the invariant it must violate or None).
const CASES: &[(&str, &str, Option<&str>)] = &[
    ("Pipeline", "Pipeline", None),
    ("Pipeline", "BrokenGuard", Some("AtMostOnce")),
    ("Pipeline", "BrokenMarker", Some("NoFalseSkip")),
    ("Pipeline", "BrokenName", Some("AcceptedOnly")),
    ("Pipeline", "BrokenWiring", Some("AcceptedOnly")),
    ("Pipeline", "BrokenCredential", Some("CredentialIsolation")),
    ("Pipeline", "BrokenBranch", Some("BranchOnce")),
    ("Session", "Session", None),
    ("Session", "BrokenSessionInputs", Some("AcceptedOnly")),
    ("Session", "BrokenSessionGuard", Some("AtMostOnce")),
    ("Session", "BrokenSessionName", Some("AtMostOnce")),
    ("Session", "BrokenSessionCeiling", Some("Ceiling")),
    (
        "Session",
        "BrokenSessionCredential",
        Some("CredentialIsolation"),
    ),
];

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether one TLC run ended as its configuration must.
fn verdict(config: &str, invariant: Option<&str>, status: Option<i32>, output: &str) -> Result<()> {
    let (want, expected) = match invariant {
        Some(invariant) => (TLC_VIOLATION, format!("Invariant {invariant} is violated")),
        None => (0, COMPLETED.to_owned()),
    };
    ensure!(
        output.len() <= MAX_OUTPUT_BYTES,
        "{config}: more than {MAX_OUTPUT_BYTES} bytes of TLC output"
    );
    ensure!(
        status == Some(want) && output.contains(&expected),
        "{config}: expected {expected}, not a tooling/parse failure (TLC exit {status:?})"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [specs] = args.as_slice() else {
        bail!("usage: specs-check SPECS_DIR");
    };
    let specs = Path::new(specs);
    let results = tempfile::Builder::new()
        .prefix("agentic-job-tlc-")
        .tempdir()
        .context("creating the results directory")?
        .keep();
    println!("Full TLC reports: {}", results.display());
    let jar = results.join(JAR_NAME);
    let download = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--retry", "2", "--max-time", "120", JAR_URL, "--output"])
        .arg(&jar)
        .status()
        .context("running curl")?;
    ensure!(download.success(), "Could not download {JAR_NAME}");
    let bytes = std::fs::read(&jar).with_context(|| format!("reading {}", jar.display()))?;
    ensure!(
        sha256_hex(&bytes) == JAR_SHA256,
        "{JAR_NAME} checksum mismatch; refusing to execute it"
    );
    let java = std::env::var_os(JAVA_BIN_ENV).unwrap_or_else(|| "java".into());
    for &(module, config, invariant) in CASES {
        let run = Command::new(&java)
            .args(["-XX:+UseParallelGC", "-Xmx512m", "-cp"])
            .arg(&jar)
            .args([
                "tlc2.TLC", "-workers", "1", "-seed", "1", "-fp", "0", "-config",
            ])
            .arg(specs.join(format!("{config}.cfg")))
            .arg("-metadir")
            .arg(results.join(config))
            .arg(specs.join(format!("{module}.tla")))
            .output()
            .with_context(|| format!("{config}: running {}", java.to_string_lossy()))?;
        let output = String::from_utf8_lossy(&run.stdout) + String::from_utf8_lossy(&run.stderr);
        std::fs::write(results.join(format!("{config}.log")), output.as_bytes())
            .with_context(|| format!("writing the {config} report"))?;
        println!("{config}: TLC exit {:?}", run.status.code());
        println!("{output}");
        verdict(config, invariant, run.status.code(), &output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_expected_ending_passes() {
        let violated = "Error: Invariant AtMostOnce is violated.";
        for (invariant, status, output, ok) in [
            (
                None,
                Some(0),
                "Model checking completed. No error has been found.",
                true,
            ),
            (None, Some(12), violated, false),
            (None, Some(0), "", false),
            (None, Some(150), "parse error", false),
            (None, None, COMPLETED, false),
            (Some("AtMostOnce"), Some(12), violated, true),
            (Some("AtMostOnce"), Some(0), violated, false),
            (
                Some("AtMostOnce"),
                Some(12),
                "Invariant Ceiling is violated",
                false,
            ),
            (Some("AtMostOnce"), Some(150), violated, false),
            (Some("AtMostOnce"), Some(12), COMPLETED, false),
        ] {
            let got = verdict("c", invariant, status, output);
            assert_eq!(
                got.is_ok(),
                ok,
                "{invariant:?} {status:?} {output:?}: {got:?}"
            );
        }
        let long = format!("{COMPLETED}{}", "x".repeat(MAX_OUTPUT_BYTES));
        assert!(verdict("c", None, Some(0), &long).is_err());
    }

    /// Every case names a module and configuration in specs/, and every
    /// configuration there is checked.
    #[test]
    fn cases_are_the_specs_configurations() {
        let specs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
        let mut configs: Vec<String> = std::fs::read_dir(&specs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter_map(|name| name.strip_suffix(".cfg").map(str::to_owned))
            .collect();
        configs.sort();
        let mut cased: Vec<String> = CASES.iter().map(|(_, c, _)| (*c).to_owned()).collect();
        cased.sort();
        assert_eq!(configs, cased);
        for (module, _, _) in CASES {
            assert!(specs.join(format!("{module}.tla")).is_file(), "{module}");
        }
    }

    #[test]
    fn hashes_as_sha256() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
