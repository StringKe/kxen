//! JSON-RPC 请求的语法、版本、options 与系统方法校验。

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::protocol::{
    INVALID_PARAMS, INVALID_REQUEST, M_HEARTBEAT, M_SUBSCRIBE, M_UNSUBSCRIBE, PARSE_ERROR, Request, RequestOptions, RequestVersion,
    Response, STREAM_NOT_FOUND, value_kind,
};

pub(super) enum SystemAction {
    Heartbeat,
    Subscribe(HashSet<String>),
    Unsubscribe(String),
}

type RequestResult<T> = Result<T, Box<Response>>;

pub(super) fn parse(text: &str) -> RequestResult<Request> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        Box::new(Response::err_with_data(Value::Null, PARSE_ERROR, "parse error", json!({ "detail": error.to_string() })))
    })?;
    let object = value.as_object().ok_or_else(|| {
        Box::new(Response::err_with_data(
            Value::Null,
            INVALID_REQUEST,
            "invalid request",
            json!({ "field": "$", "expected": "object", "received": value_kind(&value) }),
        ))
    })?;
    let id = valid_id(object)?;
    let version = parse_version(object, &id)?;
    let method = match object.get("method") {
        Some(Value::String(method)) if !method.is_empty() => method.clone(),
        value => return Err(field_error(id, INVALID_REQUEST, "method", "non-empty string", value)),
    };
    let params = match object.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value.clone(),
        value => return Err(field_error(id, INVALID_PARAMS, "params", "object", value)),
    };
    let options = parse_options(object, &id)?;
    Ok(Request { id, method, params, options, version })
}

fn valid_id(object: &Map<String, Value>) -> RequestResult<Value> {
    match object.get("id") {
        Some(value @ (Value::String(_) | Value::Number(_) | Value::Null)) => Ok(value.clone()),
        value => Err(field_error(Value::Null, INVALID_REQUEST, "id", "string, number, or null", value)),
    }
}

fn parse_version(object: &Map<String, Value>, id: &Value) -> RequestResult<RequestVersion> {
    match object.get("jsonrpc") {
        None => Ok(RequestVersion::Compat2),
        Some(Value::String(version)) if version == "2.0" => Ok(RequestVersion::Compat2),
        Some(Value::String(version)) if version == "3.0" => Ok(RequestVersion::V3),
        value => Err(field_error(id.clone(), INVALID_REQUEST, "jsonrpc", "2.0 or 3.0", value)),
    }
}

fn parse_options(object: &Map<String, Value>, id: &Value) -> RequestResult<RequestOptions> {
    let options = match object.get("options") {
        None | Some(Value::Null) => return Ok(RequestOptions::default()),
        Some(Value::Object(options)) => options,
        value => return Err(field_error(id.clone(), INVALID_PARAMS, "options", "object", value)),
    };
    let stream = match options.get("stream") {
        None => None,
        Some(Value::Bool(stream)) => Some(*stream),
        value => return Err(field_error(id.clone(), INVALID_PARAMS, "options.stream", "boolean", value)),
    };
    Ok(RequestOptions { stream })
}

fn field_error(id: Value, code: i64, field: &str, expected: &str, value: Option<&Value>) -> Box<Response> {
    Box::new(Response::err_with_data(
        id,
        code,
        if code == INVALID_REQUEST { "invalid request" } else { "invalid params" },
        json!({ "field": field, "expected": expected, "received": value.map(value_kind).unwrap_or("missing") }),
    ))
}

pub(super) fn validate_system(request: &Request, stream_ids: &[String]) -> RequestResult<Option<SystemAction>> {
    if request.method != M_SUBSCRIBE && request.options.stream == Some(true) {
        return Err(field_error(request.id.clone(), INVALID_PARAMS, "options.stream", "false or omitted", Some(&Value::Bool(true))));
    }
    match request.method.as_str() {
        M_HEARTBEAT => Ok(Some(SystemAction::Heartbeat)),
        M_SUBSCRIBE => {
            if request.version == RequestVersion::V3 && request.options.stream != Some(true) {
                let received = request.options.stream.map(Value::Bool);
                return Err(field_error(request.id.clone(), INVALID_PARAMS, "options.stream", "true", received.as_ref()));
            }
            let topics = string_array(request, "topics")?;
            if topics.is_empty() {
                return Err(field_error(request.id.clone(), INVALID_PARAMS, "topics", "non-empty string array", None));
            }
            if let Some(topic) = topics.iter().find(|topic| !valid_topic(topic)) {
                return Err(Box::new(Response::err_with_data(
                    request.id.clone(),
                    INVALID_PARAMS,
                    "invalid subscription topic",
                    json!({ "field": "topics", "topic": topic }),
                )));
            }
            Ok(Some(SystemAction::Subscribe(topics.into_iter().collect())))
        }
        M_UNSUBSCRIBE => {
            let stream_id = required_string(request, "stream_id")?;
            if !stream_ids.iter().any(|id| id == stream_id) {
                return Err(Box::new(Response::err_with_data(
                    request.id.clone(),
                    STREAM_NOT_FOUND,
                    "stream not found",
                    json!({ "stream_id": stream_id }),
                )));
            }
            Ok(Some(SystemAction::Unsubscribe(stream_id.to_string())))
        }
        _ => Ok(None),
    }
}

fn valid_topic(topic: &str) -> bool {
    matches!(topic, "llm.delta" | "approval.global" | "task.update" | "goal.update" | "notification" | "session.update")
        || topic.strip_prefix("session:").is_some_and(|id| !id.is_empty() && !id.chars().any(char::is_whitespace))
}

fn required_string<'a>(request: &'a Request, field: &str) -> RequestResult<&'a str> {
    request
        .params
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| field_error(request.id.clone(), INVALID_PARAMS, field, "non-empty string", request.params.get(field)))
}

fn string_array(request: &Request, field: &str) -> RequestResult<Vec<String>> {
    let Some(values) = request.params.get(field).and_then(Value::as_array) else {
        return Err(field_error(request.id.clone(), INVALID_PARAMS, field, "string array", request.params.get(field)));
    };
    if values.iter().any(|value| value.as_str().is_none_or(str::is_empty)) {
        return Err(field_error(request.id.clone(), INVALID_PARAMS, field, "string array", request.params.get(field)));
    }
    Ok(values.iter().filter_map(Value::as_str).map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> Value {
        serde_json::to_value(parse(text).expect_err("frame must be rejected")).unwrap()
    }

    #[test]
    fn distinguishes_parse_request_and_params_errors() {
        assert_eq!(error("{")["error"]["code"], PARSE_ERROR);
        assert_eq!(error("[]")["error"]["code"], INVALID_REQUEST);
        assert_eq!(error(r#"{"jsonrpc":"1.0","id":7,"method":"doctor"}"#)["error"]["code"], INVALID_REQUEST);
        assert_eq!(error(r#"{"jsonrpc":"3.0","id":7,"method":"doctor","params":[]}"#)["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn accepts_v3_v2_and_legacy_requests() {
        for frame in [
            r#"{"jsonrpc":"3.0","id":"a","method":"doctor"}"#,
            r#"{"jsonrpc":"2.0","id":"a","method":"doctor"}"#,
            r#"{"id":"a","method":"doctor"}"#,
        ] {
            assert_eq!(parse(frame).unwrap().params, json!({}));
        }
    }

    #[test]
    fn validates_stream_options_and_subscription_topics() {
        let missing = parse(r#"{"jsonrpc":"3.0","id":1,"method":"rpc.subscribe","params":{"topics":["llm.delta"]}}"#).unwrap();
        assert!(validate_system(&missing, &[]).is_err());
        let invalid =
            parse(r#"{"jsonrpc":"3.0","id":1,"method":"rpc.subscribe","params":{"topics":["not.real"]},"options":{"stream":true}}"#)
                .unwrap();
        assert!(validate_system(&invalid, &[]).is_err());
        let valid = parse(
            r#"{"jsonrpc":"3.0","id":1,"method":"rpc.subscribe","params":{"topics":["llm.delta","approval.global","session:s1"]},"options":{"stream":true}}"#,
        )
        .unwrap();
        assert!(validate_system(&valid, &[]).is_ok());
    }
}
