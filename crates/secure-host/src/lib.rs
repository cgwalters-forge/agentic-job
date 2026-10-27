//! secure-host: secure a CI host for untrusted steps, and prove it.
//!
//! The host's side of agentic-job, for any job that runs code it does not
//! trust: it knows nothing of agents, inference or safe outputs, and
//! agentic-job depends on it, never the other way round. It holds the
//! parts of the host setup that stand on their own so far, the network
//! rules, the egress proxy and the helper among them; the rest (setting
//! up the sandbox user and the probes) still lives in agentic-job's
//! `sandbox` and `secure_host` modules, which re-export these at their
//! old paths. docs/layout.md has the map.

pub mod apt;
pub mod config;
pub mod egress;
pub mod enter;
pub mod files;
pub mod helper;
pub mod host;
pub mod local;
pub mod network;
pub mod platform;
pub mod proxy;
pub mod root;
pub mod view;
pub mod worker_diagnostics;
pub mod world_write;
