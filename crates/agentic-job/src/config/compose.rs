//! `agentic-job config`: write a run's `--config` file from a caller's
//! file and from settings given one at a time.
//!
//! It is for a job that gets its settings as strings from someone else,
//! the inputs of a workflow for one. Such a string is only ever a value
//! here: it is never read as TOML, so whatever it holds (a quote, a
//! newline, a table header) it sets the one key it was given for and no
//! other. A job that pasted its inputs into a file could not say that.
//!
//! The result is parsed as `sandbox setup` and `run` will parse it, and
//! held to what `sandbox setup` checks, so a key that does not exist or
//! a value of the wrong type stops the job here, before a machine is set
//! up for it. What only `run` checks (that a run has a timeout and a
//! cap, that `github-oidc` has an audience) still stops it there.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use toml::{Table, Value};

use super::Config;
use crate::exit::Exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The configuration to start from (TOML); none is an empty one
    #[arg(long, value_name = "FILE")]
    pub from: Option<PathBuf>,
    /// Set a string, as TABLE.KEY=VALUE; an empty VALUE sets nothing
    #[arg(long, value_name = "KEY=VALUE", value_parser = Setting::parse, allow_hyphen_values = true)]
    pub string: Vec<Setting>,
    /// Set a whole number likewise
    #[arg(long, value_name = "KEY=VALUE", value_parser = Setting::parse, allow_hyphen_values = true)]
    pub integer: Vec<Setting>,
    /// Set a list of strings likewise, from words with spaces or newlines between them
    #[arg(long, value_name = "KEY=VALUE", value_parser = Setting::parse, allow_hyphen_values = true)]
    pub list: Vec<Setting>,
}

/// One `KEY=VALUE` of the command line: the tables down to the key, and
/// the text to set it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    path: Vec<String>,
    value: String,
}

impl Setting {
    /// Splits at the first `=`: everything after it is the value, more
    /// `=` included. The key is `TABLE.KEY`, of the letters, digits, `-`
    /// and `_` that every key of the configuration is made of.
    fn parse(text: &str) -> Result<Self> {
        let (key, value) = text
            .split_once('=')
            .with_context(|| format!("{text:?} is not KEY=VALUE"))?;
        let path: Vec<String> = key.split('.').map(str::to_owned).collect();
        let plain = |part: &String| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        ensure!(
            path.len() >= 2 && path.iter().all(plain),
            "{key:?} is not TABLE.KEY"
        );
        Ok(Self {
            path,
            value: value.to_owned(),
        })
    }

    fn key(&self) -> String {
        self.path.join(".")
    }
}

/// What a setting's text is turned into.
#[derive(Debug, Clone, Copy)]
enum Kind {
    String,
    Integer,
    List,
}

impl Kind {
    /// The value TEXT stands for, or none for a text that sets nothing:
    /// an input of a workflow that was left out arrives as an empty
    /// string, and must leave the caller's file as it is.
    fn value(self, setting: &Setting) -> Result<Option<Value>> {
        let text = &setting.value;
        Ok(match self {
            _ if text.trim().is_empty() => None,
            Self::String => Some(Value::String(text.clone())),
            Self::Integer => {
                let number: u32 = text.trim().parse().with_context(|| {
                    format!("{}: {text:?} is not a whole number", setting.key())
                })?;
                Some(Value::Integer(number.into()))
            }
            Self::List => Some(Value::Array(
                text.split_whitespace()
                    .map(|word| Value::String(word.to_owned()))
                    .collect(),
            )),
        })
    }
}

/// Puts VALUE at PATH of TABLE, in place of what is there, making the
/// tables on the way.
fn set(table: &mut Table, path: &[String], value: Value) -> Result<()> {
    let Some((key, tables)) = path.split_last() else {
        bail!("a setting with no key");
    };
    let mut at = table;
    for name in tables {
        at = at
            .entry(name)
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .with_context(|| format!("{name} is not a table"))?;
    }
    at.insert(key.clone(), value);
    Ok(())
}

/// The configuration of ARGS as TOML, checked.
fn compose(args: &Args) -> Result<String> {
    let mut table = match &args.from {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        }
        None => Table::new(),
    };
    let settings = [
        (Kind::String, &args.string),
        (Kind::Integer, &args.integer),
        (Kind::List, &args.list),
    ];
    for (kind, list) in settings {
        for setting in list {
            if let Some(value) = kind.value(setting)? {
                set(&mut table, &setting.path, value)
                    .with_context(|| format!("setting {}", setting.key()))?;
            }
        }
    }
    let text = toml::to_string(&table).context("writing the configuration")?;
    // As its readers will see it: parsed from the text that is printed.
    let config = Config::parse(&text)?;
    config.sandbox.validate()?;
    config.setup.validate()?;
    Ok(text)
}

pub fn run(args: &Args) -> Result<Exit> {
    let text = compose(args)?;
    std::io::stdout()
        .write_all(text.as_bytes())
        .context("writing the configuration to standard output")?;
    Ok(Exit::Success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Register;

    fn settings(list: &[&str]) -> Vec<Setting> {
        list.iter()
            .map(|text| Setting::parse(text).unwrap_or_else(|err| panic!("{text:?}: {err:#}")))
            .collect()
    }

    fn args(from: Option<&str>, string: &[&str], integer: &[&str], list: &[&str]) -> Args {
        Args {
            from: from.map(PathBuf::from),
            string: settings(string),
            integer: settings(integer),
            list: settings(list),
        }
    }

    fn composed(args: &Args) -> Config {
        let text = compose(args).unwrap_or_else(|err| panic!("{args:?}: {err:#}"));
        Config::parse(&text).unwrap()
    }

    #[test]
    fn settings_fill_their_tables() {
        let config = composed(&args(
            None,
            &[
                "inference.url=http://100.64.0.1:18080",
                "inference.register=github-oidc",
                "agent.name=fake",
                "sandbox.check.control-url=https://example.com/",
            ],
            &["limits.timeout-minutes= 75 ", "limits.budget=500"],
            &["setup.packages=just  gcc-c++\njq"],
        ));
        assert_eq!(config.inference.url, "http://100.64.0.1:18080");
        assert_eq!(config.inference.register, Some(Register::GithubOidc));
        assert_eq!(config.agent.name, "fake");
        assert_eq!(config.sandbox.check.control_url, "https://example.com/");
        assert_eq!(config.limits.timeout_minutes, 75);
        assert_eq!(config.limits.budget, 500);
        assert_eq!(config.setup.packages, ["just", "gcc-c++", "jq"]);
    }

    /// A setting replaces the file's key and leaves the rest; an empty
    /// one leaves the key too.
    #[test]
    fn settings_go_over_the_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            "[sandbox]\nstop-services = [\"docker.service\"]\n[agent]\nname = \"claude\"\n\
             model = \"opus\"\n[limits]\ntimeout-minutes = 30\nbudget = 100\n\
             [setup]\npackages = [\"jq\"]\n"
        )
        .unwrap();
        let from = file.path().to_str().unwrap();
        let config = composed(&args(
            Some(from),
            &["agent.name=fake", "agent.model=", "inference.url= "],
            &["limits.timeout-minutes=5", "limits.budget="],
            &["setup.packages="],
        ));
        assert_eq!(config.sandbox.stop_services, ["docker.service"]);
        assert_eq!(config.agent.name, "fake");
        assert_eq!(config.agent.model.as_deref(), Some("opus"));
        assert_eq!(config.inference.url, "");
        assert_eq!(
            (config.limits.timeout_minutes, config.limits.budget),
            (5, 100)
        );
        assert_eq!(config.setup.packages, ["jq"]);
    }

    /// Text that would add keys if it were pasted into a file is the
    /// value of its one key, to the character.
    #[test]
    fn a_value_is_never_read_as_toml() {
        let hostile = [
            "opus\"\n[sandbox]\nuser = \"root",
            "x\"\nallow-existing-user = true\ny = \"",
            "'''\n[limits]\nuncapped = true\n'''",
            "a = b = c",
            "\\\" \\u0000 \u{7f} \t",
        ];
        for text in hostile {
            let config = composed(&args(None, &[&format!("agent.model={text}")], &[], &[]));
            assert_eq!(config.agent.model.as_deref(), Some(text));
            let rest = Config {
                agent: Default::default(),
                ..config
            };
            assert_eq!(rest, Config::default(), "{text:?} set another key");
        }
    }

    #[test]
    fn what_is_refused() {
        let key = |text: &str| Setting::parse(text).is_err();
        for text in [
            "no-equals",
            "=x",
            "model=x",
            "agent.=x",
            ".model=x",
            "agent.mo del=x",
            "agent.\"model\"=x",
            "agent.model\n[sandbox]=x",
        ] {
            assert!(key(text), "{text:?} was taken as a setting");
        }
        let cases = [
            // (strings, integers, lists, what the error says)
            (vec!["agent.nam=x"], vec![], vec![], "unknown field `nam`"),
            (
                vec!["agents.name=x"],
                vec![],
                vec![],
                "unknown field `agents`",
            ),
            (
                vec!["limits.budget=5"],
                vec![],
                vec![],
                "invalid type: string",
            ),
            (
                vec!["inference.register=oidc"],
                vec![],
                vec![],
                "unknown variant `oidc`",
            ),
            (
                vec!["agent.name=y", "agent.name.first=x"],
                vec![],
                vec![],
                "setting agent.name.first: name is not a table",
            ),
            (
                vec!["sandbox.user=root"],
                vec![],
                vec![],
                "sandbox.user: \"root\"",
            ),
            (
                vec![],
                vec!["limits.budget=-1"],
                vec![],
                "limits.budget: \"-1\" is not a whole number",
            ),
            (
                vec![],
                vec!["limits.budget=1.5"],
                vec![],
                "limits.budget: \"1.5\" is not a whole number",
            ),
            (
                vec![],
                vec!["limits.budget=ten"],
                vec![],
                "limits.budget: \"ten\" is not a whole number",
            ),
            (
                vec![],
                vec![],
                vec!["setup.packages=jq --installroot=/"],
                "setup.packages: \"--installroot=/\" is not a package name",
            ),
            (
                vec![],
                vec![],
                vec!["agent.name=fake"],
                "invalid type: sequence",
            ),
        ];
        for (string, integer, list, names) in cases {
            let args = args(None, &string, &integer, &list);
            let err = compose(&args).expect_err("a bad setting was taken");
            assert!(format!("{err:#}").contains(names), "{args:?}: {err:#}");
        }
    }
}
