//! gh-aw's safe-output contract, as a library: the request vocabulary
//! ([`vocabulary`]) and the collector that validates and sanitizes an
//! `outputs.jsonl` as gh-aw's does ([`collect`]).
//!
//! It is given text and settings, and reads no file, environment or
//! credential: event admission, run identity, artifacts, checkpoints and
//! credential acquisition belong to its caller (agentic-job's `collect`,
//! `check` and `handback check`). docs/safe-outputs.md#library-boundary
//! lists what moved here and what stays.

pub mod collect;
pub mod vocabulary;

pub use vocabulary::*;
