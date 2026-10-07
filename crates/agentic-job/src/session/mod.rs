//! The ACP session: one agent process, driven over the Agent Client
//! Protocol, recorded, and held to its limits. Step 3 of docs/plan.md
//! brings it in from the old tree's `harness/`.
//!
//! It is a library with no command of its own: `run` drives it. A session
//! keeps a list of attached clients, which is empty for a task run, so
//! that interactive use (step 11) is the same code with a client attached.
