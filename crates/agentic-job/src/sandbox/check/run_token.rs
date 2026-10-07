//! The probes of the run token: where the sandbox user holds it, and
//! everywhere it must not be.

use anyhow::{Context, Result};

use super::{CONTAINER_UID, Checker, RunToken, Want, contains};
use crate::sandbox::host;

/// Where the sandbox user can write, and so where a token handed to it
/// could have been left, besides its home and its runtime directory.
const SHARED_WRITABLE: &[&str] = &["/tmp", "/var/tmp", "/dev/shm"];

/// Shorter than this is not a token anyone issued.
const TOKEN_MIN_LEN: usize = 20;

impl Checker<'_> {
    /// The run token: the runner's file is out of reach; the sandbox
    /// user's copy is its agent's configuration, mode 600 in a directory
    /// of mode 700, and nowhere else it can write or read a process's
    /// environment or command line. The token goes to the searches on
    /// standard input, never on a command line.
    pub(super) fn run_token(&mut self, token: RunToken<'_>, environs: &[u8]) -> Result<()> {
        let user = self.user().to_owned();
        let runner_file = token.runner_file.display().to_string();
        let config_file = token.agent_config.display().to_string();
        let config_dir = token
            .agent_config
            .parent()
            .context("the agent's configuration file has no directory")?
            .display()
            .to_string();
        let config_name = token
            .agent_config
            .file_name()
            .context("the agent's configuration file has no name")?
            .to_string_lossy()
            .into_owned();
        let value = std::fs::read_to_string(token.runner_file).unwrap_or_default();
        let value = value.trim();
        self.report.expect(
            Want::Succeed,
            "token-control",
            format!("{} reads the run token (control)", self.runner.name),
            value.len() >= TOKEN_MIN_LEN && !value.contains(char::is_whitespace),
        );
        let got = self.sandbox_succeeds(&["cat", "--", &runner_file])?;
        self.report.expect(
            Want::Fail,
            "token-runner-file",
            format!("{user} can't read {runner_file}"),
            got,
        );

        // An empty token would match every file.
        let needle = if value.is_empty() {
            self.canary.as_str()
        } else {
            value
        };
        let needle = format!("{needle}\n");
        let holds = self
            .sandbox(
                &["grep", "-qsF", "-f", "-", "--", &config_file],
                needle.as_bytes(),
            )?
            .success();
        self.report.expect(
            Want::Succeed,
            "token-config-control",
            format!("{user}'s {config_file} holds the run token (control)"),
            holds,
        );
        let mode = |path: &str| -> Result<String> {
            let output = host::output(&mut host::as_root(&["stat", "-c", "%U %a", "--", path])?)?;
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let (file_mode, dir_mode) = (mode(&config_file)?, mode(&config_dir)?);
        self.report.expect(
            Want::Succeed,
            "token-config-mode",
            format!("{config_file} is {user}'s, mode 600 (it is {file_mode})"),
            file_mode == format!("{user} 600"),
        );
        self.report.expect(
            Want::Succeed,
            "token-config-dir-mode",
            format!("its directory is {user}'s, mode 700 (it is {dir_mode})"),
            dir_mode == format!("{user} 700"),
        );

        let home = self.entry.home().display().to_string();
        let runtime = format!("/run/user/{}", self.entry.user().uid);
        let mut search = vec!["grep", "-rlsF", "-f", "-", "--", &home, &runtime];
        search.extend(SHARED_WRITABLE);
        let found = self.sandbox(&search, needle.as_bytes())?.stdout;
        let found = String::from_utf8_lossy(&found);
        let found: Vec<&str> = found.lines().filter(|line| !line.is_empty()).collect();
        self.report.expect(
            Want::Succeed,
            "token-files",
            format!(
                "{user} finds the token in no other file it can write to ({})",
                if found.is_empty() {
                    "nowhere".to_owned()
                } else {
                    found.join(", ")
                }
            ),
            found.contains(&config_file.as_str()) && found.iter().all(|file| *file == config_file),
        );
        let cmdlines = self
            .sandbox(
                &["sh", "-c", "cat /proc/[0-9]*/cmdline 2>/dev/null; true"],
                b"",
            )?
            .stdout;
        self.report.expect(
            Want::Fail,
            "token-processes",
            format!("no process {user} can read has the token in its environment or command line"),
            contains(environs, value.as_bytes()) || contains(&cmdlines, value.as_bytes()),
        );

        // Containers run rootless as the sandbox user, whose own uid is
        // their root; any other uid in them is a subordinate one.
        if !self.has_podman() {
            self.report
                .note("no podman on this host, so no container to read the token from");
            return Ok(());
        }
        let image = self.config.sandbox.check.container_image.clone();
        let volume = format!("{config_dir}:/config:ro");
        let inside = format!("/config/{config_name}");
        let read_as = |uid: &str| {
            self.sandbox_succeeds(&[
                "podman",
                "run",
                "--rm",
                "--security-opt",
                "label=disable",
                "--user",
                uid,
                "-v",
                &volume,
                &image,
                "cat",
                &inside,
            ])
        };
        let (as_root, as_subuid) = (read_as("0")?, read_as(CONTAINER_UID)?);
        self.report.expect(
            Want::Succeed,
            "token-container-control",
            format!("a container as its root ({user}) reads the configuration (control)"),
            as_root,
        );
        self.report.expect(
            Want::Fail,
            "token-container-subuid",
            format!("a container as subordinate uid {CONTAINER_UID} can't read the configuration"),
            as_subuid,
        );
        Ok(())
    }
}
