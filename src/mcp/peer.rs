//! The client on the other end of the line: what `initialize` declared,
//! and the requests the server sends it during a tool call.
//!
//! stdio is one line stream each way, so while a tool call waits for the
//! client's answer, [`Peer::request`] reads everything else the client
//! sends: a `ping` is answered at once, any other request is queued and
//! served after the call is answered, and `notifications/cancelled` for the
//! call ends the wait. The wait has no timeout, since the answer can be a
//! person filling in a form; it ends with the answer, the cancellation or
//! the end of stdin.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

use serde_json::{Map, Value, json};

use super::protocol::{self, Message, Notification, Request, SERVER_BUSY};
use crate::console::diag;

/// Requests queued behind a tool call that waits on the client; the next
/// one is answered [`SERVER_BUSY`].
const MAX_QUEUED_REQUESTS: usize = 16;

/// The capabilities the server can use, as the client declared them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClientCapabilities {
    /// `elicitation` declared with `form`, or empty, which means form mode.
    pub elicitation_form: bool,
}

impl ClientCapabilities {
    /// The capabilities in `initialize`'s params. One of another shape than
    /// the protocol defines reads as undeclared.
    pub(crate) fn declared_in(params: &Map<String, Value>) -> Self {
        let elicitation = params
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("elicitation"))
            .and_then(Value::as_object);
        Self {
            elicitation_form: elicitation
                .is_some_and(|modes| modes.contains_key("form") || !modes.contains_key("url")),
        }
    }
}

/// What the session keeps of the client between messages.
#[derive(Debug, Default)]
pub(crate) struct Client {
    /// `None` until `initialize`.
    capabilities: Option<ClientCapabilities>,
    /// The id of the server's last request; each one takes the next.
    last_request_id: u64,
    queued: VecDeque<Request>,
}

impl Client {
    pub(crate) fn initialize(&mut self, capabilities: ClientCapabilities) {
        self.capabilities = Some(capabilities);
    }

    pub(crate) const fn is_initialized(&self) -> bool {
        self.capabilities.is_some()
    }

    /// The oldest request that arrived while a tool call waited.
    pub(crate) fn next_queued(&mut self) -> Option<Request> {
        self.queued.pop_front()
    }
}

/// A request the server sends the client.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ServerMethod {
    /// `elicitation/create` in form mode: a form the person fills in.
    CreateElicitation,
}

impl ServerMethod {
    const fn name(self) -> &'static str {
        match self {
            Self::CreateElicitation => "elicitation/create",
        }
    }

    /// The capability `initialize` must have declared, as messages name it.
    const fn capability(self) -> &'static str {
        match self {
            Self::CreateElicitation => "`elicitation` with form mode",
        }
    }

    const fn supported_by(self, capabilities: ClientCapabilities) -> bool {
        match self {
            Self::CreateElicitation => capabilities.elicitation_form,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PeerError {
    #[error(
        "the client did not declare {capability} in `initialize`, so `{method}` cannot be sent"
    )]
    Unsupported {
        method: &'static str,
        capability: &'static str,
    },

    #[error("the client closed its input before answering `{0}`")]
    Closed(&'static str),

    /// The client cancelled the tool call; the call's response is not sent.
    #[error("the client cancelled the tool call")]
    Cancelled,

    #[error("the client answered `{method}` with error {code}: {message}")]
    Refused {
        method: &'static str,
        code: i64,
        message: String,
    },

    #[error("the connection to the client failed: {0}")]
    Io(#[source] io::Error),
}

/// The client, as one tool call reaches it: the line stream both ways and
/// the session's record of the client.
pub(crate) struct Peer<'a> {
    reader: &'a mut dyn BufRead,
    writer: &'a mut dyn Write,
    client: &'a mut Client,
    /// The id of the `tools/call` request this peer serves.
    call: &'a Value,
    cancelled: bool,
}

impl<'a> Peer<'a> {
    pub(crate) fn new(
        reader: &'a mut dyn BufRead,
        writer: &'a mut dyn Write,
        client: &'a mut Client,
        call: &'a Value,
    ) -> Self {
        Self {
            reader,
            writer,
            client,
            call,
            cancelled: false,
        }
    }

    /// Whether the client cancelled the call while it waited.
    pub(crate) const fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// Send `method` with `params` and wait for the client's answer, its
    /// `result` on success. Refused without a request when the client did
    /// not declare the capability `method` needs.
    pub(crate) fn request(
        &mut self,
        method: ServerMethod,
        params: &Value,
    ) -> Result<Value, PeerError> {
        if !method.supported_by(self.client.capabilities.unwrap_or_default()) {
            return Err(PeerError::Unsupported {
                method: method.name(),
                capability: method.capability(),
            });
        }
        self.client.last_request_id = self.client.last_request_id.saturating_add(1);
        let id = Value::from(self.client.last_request_id);
        protocol::write_request(&mut *self.writer, &id, method.name(), params)
            .map_err(PeerError::Io)?;
        self.await_answer(&id, method)
    }

    /// Read the line until the answer to request `id` arrives, serving
    /// everything else on it as the module describes.
    fn await_answer(&mut self, id: &Value, method: ServerMethod) -> Result<Value, PeerError> {
        loop {
            let message = protocol::read_message(&mut *self.reader)
                .map_err(PeerError::Io)?
                .ok_or(PeerError::Closed(method.name()))?;
            match message {
                Ok(Message::Response(response)) if response.id == *id => {
                    return response.outcome.map_err(|error| PeerError::Refused {
                        method: method.name(),
                        code: error.code,
                        message: error.message,
                    });
                }
                Ok(Message::Response(response)) => diag!(
                    "trurlic: ignoring a response to request {}, which nothing waits for",
                    response.id
                ),
                Ok(Message::Notification(notification)) => {
                    if self.cancels_call(&notification) {
                        self.cancelled = true;
                        return Err(PeerError::Cancelled);
                    }
                }
                Ok(Message::Request(request)) => self.hold(request).map_err(PeerError::Io)?,
                Err(malformed) => protocol::write_error(
                    &mut *self.writer,
                    &malformed.id,
                    malformed.code,
                    &malformed.message,
                )
                .map_err(PeerError::Io)?,
            }
        }
    }

    /// `true` when `notification` cancels this call. One that cancels a
    /// queued request drops it, so it is never answered.
    fn cancels_call(&mut self, notification: &Notification) -> bool {
        if notification.method != "notifications/cancelled" {
            return false;
        }
        let Some(target) = notification
            .params
            .as_ref()
            .and_then(|params| params.get("requestId"))
        else {
            return false;
        };
        if target == self.call {
            return true;
        }
        self.client.queued.retain(|request| &request.id != target);
        false
    }

    /// Answer a `ping` now and queue any other request, up to the limit.
    fn hold(&mut self, request: Request) -> io::Result<()> {
        if request.method == "ping" {
            return protocol::write_success(&mut *self.writer, &request.id, &json!({}));
        }
        if self.client.queued.len() >= MAX_QUEUED_REQUESTS {
            return protocol::write_error(
                &mut *self.writer,
                &request.id,
                SERVER_BUSY,
                &format!(
                    "{MAX_QUEUED_REQUESTS} requests already wait for the tool call in \
                     progress; send this one again once it is answered"
                ),
            );
        }
        self.client.queued.push_back(request);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALL: u64 = 100;

    fn client(elicitation: Value) -> Client {
        let params = json!({ "capabilities": { "elicitation": elicitation } });
        let mut client = Client::default();
        client.initialize(ClientCapabilities::declared_in(params.as_object().unwrap()));
        client
    }

    fn lines(messages: &[Value]) -> Vec<u8> {
        messages
            .iter()
            .flat_map(|message| format!("{message}\n").into_bytes())
            .collect()
    }

    /// Run one elicitation request against a client that sends `input`;
    /// the outcome and every line the server wrote.
    fn ask(client: &mut Client, input: &[Value]) -> (Result<Value, PeerError>, Vec<Value>) {
        let input = lines(input);
        let mut reader = &input[..];
        let mut output = Vec::new();
        let call = json!(CALL);
        let outcome = Peer::new(&mut reader, &mut output, client, &call)
            .request(ServerMethod::CreateElicitation, &json!({ "message": "?" }));
        let written = output
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        (outcome, written)
    }

    fn answer(id: u64) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "accept" } })
    }

    #[test]
    fn declared_capabilities_follow_the_protocol() {
        let cases = [
            (json!({}), true),
            (json!({ "form": {} }), true),
            (json!({ "form": {}, "url": {} }), true),
            (json!({ "url": {} }), false),
            (json!(true), false),
        ];
        for (elicitation, form) in cases {
            let params = json!({ "capabilities": { "elicitation": elicitation } });
            let declared = ClientCapabilities::declared_in(params.as_object().unwrap());
            assert_eq!(declared.elicitation_form, form, "{elicitation}");
        }
        let none =
            ClientCapabilities::declared_in(json!({ "capabilities": {} }).as_object().unwrap());
        assert!(!none.elicitation_form);
    }

    #[test]
    fn the_answer_to_the_request_is_its_result() {
        let mut client = client(json!({}));
        let (outcome, written) = ask(&mut client, &[answer(1)]);
        assert_eq!(outcome.unwrap(), json!({ "action": "accept" }));
        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["method"], "elicitation/create");
        assert_eq!(written[0]["id"], 1);
        assert_eq!(written[0]["params"]["message"], "?");
    }

    #[test]
    fn each_request_takes_the_next_id() {
        let mut client = client(json!({}));
        ask(&mut client, &[answer(1)]).0.unwrap();
        let (outcome, written) = ask(&mut client, &[answer(1), answer(2)]);
        assert_eq!(written[0]["id"], 2);
        assert!(outcome.is_ok());
    }

    #[test]
    fn a_client_without_the_capability_gets_no_request() {
        let mut client = client(json!({ "url": {} }));
        let (outcome, written) = ask(&mut client, &[answer(1)]);
        assert!(
            matches!(outcome, Err(PeerError::Unsupported { .. })),
            "{outcome:?}"
        );
        assert!(written.is_empty());

        let (outcome, _) = ask(&mut Client::default(), &[answer(1)]);
        assert!(
            matches!(outcome, Err(PeerError::Unsupported { .. })),
            "{outcome:?}"
        );
    }

    /// While the call waits, a ping is answered at once, other requests
    /// queue in order, a stray response and a notification change nothing,
    /// and a malformed line gets its error.
    #[test]
    fn the_wait_serves_the_rest_of_the_line() {
        let mut client = client(json!({}));
        let (outcome, written) = ask(
            &mut client,
            &[
                json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" }),
                json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 99, "result": {} }),
                json!({ "jsonrpc": "2.0", "id": 8 }),
                json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call" }),
                answer(1),
            ],
        );
        assert!(outcome.is_ok());
        assert_eq!(
            written[1],
            json!({ "jsonrpc": "2.0", "id": "p", "result": {} })
        );
        assert_eq!(written[2]["id"], 8);
        assert_eq!(written[2]["error"]["code"], protocol::INVALID_REQUEST);
        assert_eq!(written.len(), 3);
        let queued: Vec<Value> = std::iter::from_fn(|| client.next_queued())
            .map(|request| request.id)
            .collect();
        assert_eq!(queued, [json!(7), json!(9)]);
    }

    #[test]
    fn a_request_past_the_queue_limit_is_answered_busy() {
        let mut client = client(json!({}));
        let mut input: Vec<Value> = (0..=MAX_QUEUED_REQUESTS)
            .map(|id| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" }))
            .collect();
        input.push(answer(1));
        let (outcome, written) = ask(&mut client, &input);
        assert!(outcome.is_ok());
        assert_eq!(written.len(), 2);
        assert_eq!(written[1]["id"], MAX_QUEUED_REQUESTS);
        assert_eq!(written[1]["error"]["code"], SERVER_BUSY);
        assert_eq!(client.queued.len(), MAX_QUEUED_REQUESTS);
    }

    #[test]
    fn cancelling_the_call_ends_the_wait() {
        let mut client = client(json!({}));
        let input = lines(&[json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": CALL, "reason": "user" }
        })]);
        let mut reader = &input[..];
        let mut output = Vec::new();
        let call = json!(CALL);
        let mut peer = Peer::new(&mut reader, &mut output, &mut client, &call);
        let outcome = peer.request(ServerMethod::CreateElicitation, &json!({}));
        assert!(matches!(outcome, Err(PeerError::Cancelled)), "{outcome:?}");
        assert!(peer.cancelled());
    }

    #[test]
    fn cancelling_a_queued_request_drops_it() {
        let mut client = client(json!({}));
        let (outcome, _) = ask(
            &mut client,
            &[
                json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }),
                json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 7 } }),
                answer(1),
            ],
        );
        assert!(outcome.is_ok());
        assert!(client.next_queued().is_none());
    }

    #[test]
    fn the_end_of_input_ends_the_wait() {
        let mut client = client(json!({}));
        let (outcome, _) = ask(&mut client, &[]);
        assert!(matches!(outcome, Err(PeerError::Closed(_))), "{outcome:?}");
    }

    #[test]
    fn an_error_answer_names_the_code() {
        let mut client = client(json!({}));
        let (outcome, _) = ask(
            &mut client,
            &[json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "no" } })],
        );
        let error = outcome.unwrap_err();
        assert!(matches!(error, PeerError::Refused { code: -32602, .. }));
        assert!(error.to_string().contains("-32602"), "{error}");
    }
}
