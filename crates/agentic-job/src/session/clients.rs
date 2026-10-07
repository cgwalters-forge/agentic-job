//! The clients attached to a session.
//!
//! A session is the agent-facing half of an ACP proxy: it is the agent's
//! client, and what it does not answer itself it can pass on. Who it
//! passes things on to is this list. For a task run the list is empty:
//! the session's own prompt is the only one, and the turn's end is the
//! session's. Interactive use (step 11 of docs/plan.md, and
//! docs/interactive.md) attaches a client to the same session.
//!
//! What is here is only the seam: every message from the agent passes
//! the transcript's tap, and the tap hands its notifications to this
//! list. Nothing can attach yet, and attaching will need more than a
//! list that is fixed when the session starts. Still to be designed, by
//! step 11: a client that joins a session already running, the way from
//! a client to the agent, who answers a permission request when a person
//! is watching, how a client's prompt is held while a turn runs (Claude
//! Code's adapter ends the running turn for one), and what ends a session
//! that has a client, since `end_turn` no longer does.
//!
//! The SDK's `Proxy` role is not what this is built on: it speaks
//! `_proxy/*` to a conductor that owns both of its connections, and a
//! task run has no client side for one to own. The session holds the
//! `Client` role toward the agent, which is the half a proxy and a task
//! run share; a client attaches with a connection of the `Agent` role on
//! the other side of this list.

use serde_json::Value;
use tokio::sync::mpsc;

/// One attached client: where the agent's messages are passed on to.
#[derive(Debug)]
struct Attached {
    to_client: mpsc::UnboundedSender<Value>,
}

/// The attached clients of one session.
#[derive(Debug, Default)]
pub struct Clients {
    attached: Vec<Attached>,
}

impl Clients {
    /// No client: a task run.
    pub fn none() -> Self {
        Self::default()
    }

    /// Passes a notification from the agent on to every client. Requests
    /// stay with the session, which answers them from its policy.
    pub(super) fn relay(&self, msg: &Value) {
        if self.attached.is_empty() || !msg["id"].is_null() || msg["method"].is_null() {
            return;
        }
        for client in &self.attached {
            // A client that went away is no reason to stop the agent.
            let _ = client.to_client.send(msg.clone());
        }
    }

    /// Attaches a client and returns what it will be sent. Only the tests
    /// can: the listener is step 11's.
    #[cfg(test)]
    fn attach(&mut self) -> mpsc::UnboundedReceiver<Value> {
        let (to_client, from_session) = mpsc::unbounded_channel();
        self.attached.push(Attached { to_client });
        from_session
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_task_run_relays_to_nobody() {
        Clients::none().relay(&json!({"method": "session/update", "params": {}}));
    }

    #[test]
    fn notifications_reach_every_client() {
        let mut clients = Clients::none();
        let mut receivers = [clients.attach(), clients.attach()];
        let update = json!({"jsonrpc": "2.0", "method": "session/update", "params": {"n": 1}});
        let messages = [
            update.clone(),
            // A request, and a response: the session's to deal with.
            json!({"jsonrpc": "2.0", "id": 1, "method": "session/request_permission"}),
            json!({"jsonrpc": "2.0", "id": 2, "result": {}}),
        ];
        for msg in &messages {
            clients.relay(msg);
        }
        for rx in &mut receivers {
            assert_eq!(rx.try_recv().ok(), Some(update.clone()));
            assert!(rx.try_recv().is_err());
        }
    }
}
