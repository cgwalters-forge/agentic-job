//! The built binary's exit states, which the unit tests cannot see.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");

/// What `exit::ERROR` is; spelled out because it is the interface.
const ERROR: i32 = 2;

#[test]
fn exit_states() {
    let cases: &[(&[&str], i32, &str)] = &[
        (&["--version"], 0, ""),
        // `sandbox check` itself depends on whether a sandbox is set up
        // where the tests run; tests/sandbox_host.rs covers it.
        (
            &["sandbox", "setup", "--config", "/nonexistent.toml"],
            ERROR,
            "/nonexistent.toml",
        ),
        (&["no-such-command"], ERROR, "unrecognized subcommand"),
        (&["run"], ERROR, "required arguments"),
    ];
    for (args, code, stderr) in cases {
        let output = Command::new(BIN).args(*args).output().unwrap();
        assert_eq!(output.status.code(), Some(*code), "{args:?}");
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains(stderr), "{args:?}: stderr was {text:?}");
    }
}
