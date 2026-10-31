//! gh-aw's safe-output contract, as a library: the request vocabulary
//! ([`vocabulary`]), the collector that validates and sanitizes an
//! `outputs.jsonl` as gh-aw's does ([`collect`]), the policy a run's
//! outputs are held to ([`contract`]) and the bounds it is compiled within
//! ([`bounds`]), and the check of a hand-back against that policy
//! ([`check`]).
//!
//! It is given text and settings, and reads no file, environment or
//! credential: event admission, compiling a request into a policy, run
//! identity, artifacts, checkpoints, memory, secret scanning and
//! credential acquisition belong to its caller (agentic-job's `policy`,
//! `event`, `collect`, `check` and `handback check`).
//! docs/safe-outputs.md#library-boundary lists what moved here and what
//! stays.

pub mod bounds;
pub mod check;
pub mod collect;
pub mod contract;
pub mod vocabulary;

pub use vocabulary::*;
