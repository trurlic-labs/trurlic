//! JSON-RPC 2.0 over newline-delimited stdio: one message read and sorted
//! into a request, a notification or a response, and one line written per
//! answer.
//!
//! Every line the client sends gets at most one answer: a request one
//! response, a line that is no message one error (carrying the `id` of a
//! line with a `method` when one can be read; a line without one would be
//! a response, whose `id` is the server's), a notification or a response
//! none. A line longer
//! than [`MAX_MESSAGE_BYTES`] is read to its end and discarded whole, so
//! its tail is never taken for a message of its own.

use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde_json::{Map, Value};

// -- Error codes (JSON-RPC 2.0 section 5.1) -------------------------------

pub(crate) const PARSE_ERROR: i32 = -32700;
pub(crate) const INVALID_REQUEST: i32 = -32600;
pub(crate) const METHOD_NOT_FOUND: i32 = -32601;
pub(crate) const INVALID_PARAMS: i32 = -32602;
pub(crate) const INTERNAL_ERROR: i32 = -32603;
/// From the range JSON-RPC leaves to the server (-32000 to -32099): a
/// request arrived while a tool call waits on the client and the requests
/// queued behind that call are already at their limit.
pub(crate) const SERVER_BUSY: i32 = -32000;

/// Bytes of one message, and of the document a tool's CLI twin reads.
pub(crate) const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

// -- Messages --------------------------------------------------------------

/// A request: `id` is a string or an integer, never `null` (MCP narrows
/// JSON-RPC there).
#[derive(Debug, PartialEq)]
pub(crate) struct Request {
    pub id: Value,
    pub method: String,
    pub params: Option<Map<String, Value>>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Notification {
    pub method: String,
    pub params: Option<Map<String, Value>>,
}

/// The client's answer to a request the server sent.
#[derive(Debug, PartialEq)]
pub(crate) struct Response {
    pub id: Value,
    pub outcome: Result<Value, RpcError>,
}

/// The `error` member of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

/// A line that holds no message, and the error that answers it.
#[derive(Debug, PartialEq)]
pub(crate) struct Malformed {
    /// The line's `id` when it is a string or an integer, else `null`.
    pub id: Value,
    pub code: i32,
    pub message: String,
}

impl Malformed {
    fn invalid(id: Value, message: impl Into<String>) -> Self {
        Self {
            id,
            code: INVALID_REQUEST,
            message: message.into(),
        }
    }
}

// -- Reading ---------------------------------------------------------------

/// The next message on `reader`, skipping blank lines; `Ok(None)` at the
/// end of the input. An `Err` is a failed read of the stream itself.
pub(crate) fn read_message(
    reader: &mut (impl BufRead + ?Sized),
) -> io::Result<Option<Result<Message, Malformed>>> {
    loop {
        let Some(line) = read_line(reader, MAX_MESSAGE_BYTES)? else {
            return Ok(None);
        };
        let Line::Complete(bytes) = line else {
            return Ok(Some(Err(Malformed::invalid(
                Value::Null,
                format!("message exceeds {MAX_MESSAGE_BYTES} bytes; the line was discarded"),
            ))));
        };
        let text = bytes.trim_ascii();
        if text.is_empty() {
            continue;
        }
        return Ok(Some(match serde_json::from_slice(text) {
            Ok(message) => classify(message),
            Err(e) => Err(Malformed {
                id: Value::Null,
                code: PARSE_ERROR,
                message: format!("parse error: {e}"),
            }),
        }));
    }
}

enum Line {
    Complete(Vec<u8>),
    /// Longer than the limit; consumed to its newline and dropped.
    Oversize,
}

/// One newline-terminated line, holding at most `limit` bytes of it in
/// memory. `Ok(None)` at the end of the input with nothing read.
fn read_line(reader: &mut (impl BufRead + ?Sized), limit: usize) -> io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut oversize = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(match (oversize, line.is_empty()) {
                (true, _) => Some(Line::Oversize),
                (false, true) => None,
                (false, false) => Some(Line::Complete(line)),
            });
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        if !oversize {
            line.extend_from_slice(available.get(..take).unwrap_or_default());
            if line.len() > limit {
                oversize = true;
                line = Vec::new();
            }
        }
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(if oversize {
                Line::Oversize
            } else {
                Line::Complete(line)
            }));
        }
    }
}

/// Where a message's `id` stands before the message is known to be valid.
enum Id {
    Absent,
    Valid(Value),
    Invalid,
}

impl Id {
    /// The `id` an error answering the message carries.
    fn into_reply(self) -> Value {
        match self {
            Self::Valid(id) => id,
            Self::Absent | Self::Invalid => Value::Null,
        }
    }
}

fn classify(message: Value) -> Result<Message, Malformed> {
    let Value::Object(mut object) = message else {
        return Err(Malformed::invalid(
            Value::Null,
            "a message is one JSON object; batches are not supported",
        ));
    };
    let id = match object.remove("id") {
        None => Id::Absent,
        Some(id) if id.is_string() || id.is_i64() || id.is_u64() => Id::Valid(id),
        Some(_) => Id::Invalid,
    };
    let method = object.remove("method");
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        // A line without `method` is no request: its `id` is from the
        // server's own id space, so the error carries `null`.
        let reply = match method {
            Some(_) => id.into_reply(),
            None => Value::Null,
        };
        return Err(Malformed::invalid(reply, "`jsonrpc` must be \"2.0\""));
    }
    match method {
        Some(method) => classify_call(method, object.remove("params"), id),
        None => classify_response(object, id),
    }
}

fn classify_call(method: Value, params: Option<Value>, id: Id) -> Result<Message, Malformed> {
    let Value::String(method) = method else {
        return Err(Malformed::invalid(
            id.into_reply(),
            "`method` must be a string",
        ));
    };
    let params = match params {
        None => None,
        Some(Value::Object(params)) => Some(params),
        Some(_) => {
            return Err(Malformed::invalid(
                id.into_reply(),
                "`params` must be an object",
            ));
        }
    };
    match id {
        Id::Absent => Ok(Message::Notification(Notification { method, params })),
        Id::Valid(id) => Ok(Message::Request(Request { id, method, params })),
        Id::Invalid => Err(Malformed::invalid(
            Value::Null,
            "`id` must be a string or an integer",
        )),
    }
}

/// A line without `method`, read as a response. Its `id` is one of the
/// server's own, so every error answering a malformed one carries `null`.
fn classify_response(mut object: Map<String, Value>, id: Id) -> Result<Message, Malformed> {
    let outcome = match (object.remove("result"), object.remove("error")) {
        (Some(result), None) => Some(Ok(result)),
        (None, Some(error)) => rpc_error(&error).map(Err),
        (Some(_), Some(_)) | (None, None) => {
            return Err(Malformed::invalid(
                Value::Null,
                "a message needs a `method`, or exactly one of `result` and `error`",
            ));
        }
    };
    let (Id::Valid(id), Some(outcome)) = (id, outcome) else {
        return Err(Malformed::invalid(
            Value::Null,
            "a response needs a string or integer `id`, and an `error` needs an integer \
             `code` and a string `message`",
        ));
    };
    Ok(Message::Response(Response { id, outcome }))
}

fn rpc_error(error: &Value) -> Option<RpcError> {
    Some(RpcError {
        code: error.get("code").and_then(Value::as_i64)?,
        message: error.get("message").and_then(Value::as_str)?.to_owned(),
    })
}

// -- Writing ---------------------------------------------------------------

pub(crate) fn write_success(
    writer: &mut (impl Write + ?Sized),
    id: &Value,
    result: &impl Serialize,
) -> io::Result<()> {
    #[derive(Serialize)]
    struct Success<'a, R> {
        jsonrpc: &'static str,
        id: &'a Value,
        result: &'a R,
    }
    write_line(
        writer,
        &Success {
            jsonrpc: "2.0",
            id,
            result,
        },
    )
}

pub(crate) fn write_error(
    writer: &mut (impl Write + ?Sized),
    id: &Value,
    code: i32,
    message: &str,
) -> io::Result<()> {
    #[derive(Serialize)]
    struct Failure<'a> {
        jsonrpc: &'static str,
        id: &'a Value,
        error: Error<'a>,
    }
    #[derive(Serialize)]
    struct Error<'a> {
        code: i32,
        message: &'a str,
    }
    write_line(
        writer,
        &Failure {
            jsonrpc: "2.0",
            id,
            error: Error { code, message },
        },
    )
}

/// A request of the server's own, answered by a [`Response`] with `id`.
pub(crate) fn write_request(
    writer: &mut (impl Write + ?Sized),
    id: &Value,
    method: &str,
    params: &Value,
) -> io::Result<()> {
    #[derive(Serialize)]
    struct Outgoing<'a> {
        jsonrpc: &'static str,
        id: &'a Value,
        method: &'a str,
        params: &'a Value,
    }
    write_line(
        writer,
        &Outgoing {
            jsonrpc: "2.0",
            id,
            method,
            params,
        },
    )
}

/// Serialize `message` in one pass into a buffer, then write it with its
/// newline: the client never sees part of a line, even when the write
/// fails or the caller unwinds.
fn write_line(writer: &mut (impl Write + ?Sized), message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

// -- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(input: &[u8]) -> Option<Result<Message, Malformed>> {
        read_message(&mut &input[..]).unwrap()
    }

    fn malformed(input: &str) -> Malformed {
        match read(input.as_bytes()) {
            Some(Err(malformed)) => malformed,
            other => panic!("{input}: {other:?}"),
        }
    }

    #[test]
    fn a_request_keeps_its_id_method_and_params() {
        let line = br#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"x"}}"#;
        let Some(Ok(Message::Request(request))) = read(line) else {
            panic!("not a request");
        };
        assert_eq!(request.id, json!("a"));
        assert_eq!(request.method, "tools/call");
        assert_eq!(request.params.unwrap()["name"], "x");
    }

    #[test]
    fn a_message_without_id_is_a_notification() {
        let line = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(matches!(read(line), Some(Ok(Message::Notification(_)))));
    }

    #[test]
    fn a_response_carries_its_result_or_its_error() {
        let result = br#"{"jsonrpc":"2.0","id":3,"result":{"action":"accept"}}"#;
        let Some(Ok(Message::Response(response))) = read(result) else {
            panic!("not a response");
        };
        assert_eq!(response.id, 3);
        assert_eq!(response.outcome, Ok(json!({"action": "accept"})));

        let error = br#"{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"no form"}}"#;
        let Some(Ok(Message::Response(response))) = read(error) else {
            panic!("not a response");
        };
        assert_eq!(
            response.outcome,
            Err(RpcError {
                code: -32602,
                message: "no form".into()
            })
        );
    }

    /// Each malformed shape is answered `-32600`, with the line's `id`
    /// when it is a string or an integer and the line has a `method`: a
    /// line without one is no request, and its `id` is the server's.
    #[test]
    fn a_malformed_message_is_an_invalid_request_with_its_id_recovered() {
        let cases = [
            (r#"{"jsonrpc":"2.0","id":7}"#, Value::Null),
            (r#"{"jsonrpc":"1.0","id":7,"result":{}}"#, Value::Null),
            (r#"{"id":7,"error":{"code":1,"message":"m"}}"#, Value::Null),
            (r#"{"jsonrpc":"1.0","id":"a","method":"ping"}"#, json!("a")),
            (r#"{"id":8,"method":"ping"}"#, json!(8)),
            (r#"{"jsonrpc":"2.0","id":9,"method":5}"#, json!(9)),
            (
                r#"{"jsonrpc":"2.0","id":10,"method":"x","params":[1]}"#,
                json!(10),
            ),
            (
                r#"{"jsonrpc":"2.0","id":11,"result":1,"error":{}}"#,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
                Value::Null,
            ),
            (r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#, Value::Null),
            (
                r#"{"jsonrpc":"2.0","id":{"n":1},"method":"ping"}"#,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":"2.0","id":12,"error":{"code":"x"}}"#,
                Value::Null,
            ),
            (r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, Value::Null),
            ("42", Value::Null),
        ];
        for (line, id) in cases {
            let answer = malformed(line);
            assert_eq!(answer.code, INVALID_REQUEST, "{line}");
            assert_eq!(answer.id, id, "{line}");
        }
    }

    #[test]
    fn a_line_that_is_not_json_is_a_parse_error() {
        let answer = malformed("not json");
        assert_eq!(answer.code, PARSE_ERROR);
        assert_eq!(answer.id, Value::Null);
    }

    #[test]
    fn blank_lines_are_skipped_and_the_end_of_input_is_none() {
        let input = b"\n  \r\n{\"jsonrpc\":\"2.0\",\"method\":\"n\"}\n";
        let mut reader = &input[..];
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Some(Ok(Message::Notification(_)))
        ));
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    /// The tail of an oversize line is consumed with it, so the next read
    /// starts at the next line and the long one is answered once.
    #[test]
    fn an_oversize_line_is_discarded_whole() {
        let mut input = vec![b'x'; 64];
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        // A buffer smaller than the line, so the line spans several reads.
        let mut reader = std::io::BufReader::with_capacity(8, &input[..]);

        assert!(matches!(
            read_line(&mut reader, 16).unwrap(),
            Some(Line::Oversize)
        ));
        let Some(Line::Complete(next)) = read_line(&mut reader, 1024).unwrap() else {
            panic!("the next line is lost");
        };
        assert!(next.starts_with(b"{\"jsonrpc\""));
    }

    #[test]
    fn an_oversize_last_line_without_newline_is_discarded_too() {
        let input = [b'x'; 64];
        let mut reader = std::io::BufReader::with_capacity(8, &input[..]);
        assert!(matches!(
            read_line(&mut reader, 16).unwrap(),
            Some(Line::Oversize)
        ));
        assert!(read_line(&mut reader, 16).unwrap().is_none());
    }

    #[test]
    fn a_line_at_the_limit_is_kept() {
        let input = b"0123456789\n";
        let Some(Line::Complete(line)) = read_line(&mut &input[..], 11).unwrap() else {
            panic!("a line at the limit was refused");
        };
        assert_eq!(line, input);
    }

    #[test]
    fn every_writer_emits_one_newline_terminated_json_line() {
        let mut output = Vec::new();
        write_success(&mut output, &json!(1), &json!({"ok": true})).unwrap();
        write_error(&mut output, &Value::Null, PARSE_ERROR, "bad").unwrap();
        write_request(&mut output, &json!(2), "elicitation/create", &json!({})).unwrap();

        let lines: Vec<Value> = output
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(output.iter().filter(|&&b| b == b'\n').count(), 3);
        assert_eq!(
            lines[0],
            json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}})
        );
        assert_eq!(
            lines[1],
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": PARSE_ERROR, "message": "bad"}})
        );
        assert_eq!(
            lines[2],
            json!({"jsonrpc": "2.0", "id": 2, "method": "elicitation/create", "params": {}})
        );
    }
}
