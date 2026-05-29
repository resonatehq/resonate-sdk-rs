use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// SHARED TYPES
// =============================================================================

/// The wire format for data crossing the durability boundary.
///
/// On the wire, `data` is a base64-encoded JSON string (or undefined).
/// Internally after decoding by the Codec, `data` holds the deserialized value.
///
/// Mirrors the TS type:
/// ```ts
/// type Value = { headers?: Record<string, string>; data?: any };
/// ```
#[derive(Debug, Clone, Default, Serialize)]
pub struct Value {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Value {
    /// Get a reference to the data field, defaulting to `serde_json::Value::Null` if absent.
    pub fn data_as_ref(&self) -> &serde_json::Value {
        static NULL: serde_json::Value = serde_json::Value::Null;
        self.data.as_ref().unwrap_or(&NULL)
    }

    /// Get a clone of the data field, defaulting to `serde_json::Value::Null` if absent.
    pub fn data_or_null(&self) -> serde_json::Value {
        self.data.clone().unwrap_or(serde_json::Value::Null)
    }

    /// Consume self and return the data field, defaulting to `serde_json::Value::Null` if absent.
    pub fn into_data_or_null(self) -> serde_json::Value {
        self.data.unwrap_or(serde_json::Value::Null)
    }

    /// Get headers, defaulting to empty map if absent.
    pub fn headers_or_empty(&self) -> HashMap<String, String> {
        self.headers.clone().unwrap_or_default()
    }

    /// Serialize any value into a `Value`.
    pub fn from_serializable<T: Serialize>(val: T) -> crate::error::Result<Self> {
        Ok(Self {
            headers: None,
            data: Some(serde_json::to_value(val)?),
        })
    }

    /// Deserialize the data field into `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> crate::error::Result<T> {
        T::deserialize(self.data_as_ref()).map_err(Into::into)
    }

    /// Consume self and deserialize data into `T`.
    pub fn into_decoded<T: DeserializeOwned>(self) -> crate::error::Result<T> {
        serde_json::from_value(self.into_data_or_null()).map_err(Into::into)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Null => Ok(Value::default()),
            serde_json::Value::Object(map) => {
                let headers: Option<HashMap<String, String>> = map
                    .get("headers")
                    .and_then(|h| serde_json::from_value(h.clone()).ok());
                let data = map.get("data").cloned();
                Ok(Value { headers, data })
            }
            // If it's not an object, treat the raw value as `data`
            other => Ok(Value {
                headers: None,
                data: Some(other),
            }),
        }
    }
}

// =============================================================================
// RECORDS
// =============================================================================

/// The state of a durable promise.
///
/// Mirrors the TS type:
/// ```ts
/// state: "pending" | "resolved" | "rejected" | "rejected_canceled" | "rejected_timedout"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromiseState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "resolved")]
    Resolved,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "rejected_canceled")]
    RejectedCanceled,
    #[serde(rename = "rejected_timedout")]
    RejectedTimedout,
}

/// A durable promise record as stored by the server.
///
/// Mirrors the TS type:
/// ```ts
/// type PromiseRecord = {
///   id: string; state: PromiseState; param: Value; value: Value;
///   tags: Record<string, string>; timeoutAt: number;
///   createdAt: number; settledAt?: number;
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromiseRecord {
    pub id: String,
    pub state: PromiseState,
    #[serde(default)]
    pub param: Value,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    pub timeout_at: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub settled_at: Option<i64>,
}

/// The state of a task.
///
/// Mirrors the TS type:
/// ```ts
/// state: "pending" | "acquired" | "suspended" | "halted" | "fulfilled"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "acquired")]
    Acquired,
    #[serde(rename = "suspended")]
    Suspended,
    #[serde(rename = "halted")]
    Halted,
    #[serde(rename = "fulfilled")]
    Fulfilled,
}

/// A task record as returned by the server.
///
/// Mirrors the TS type:
/// ```ts
/// type TaskRecord = {
///   id: string;
///   state: "pending" | "acquired" | "suspended" | "halted" | "fulfilled";
///   version: number;
///   resumes: string[] | number | boolean;
///   ttl?: number;
///   pid?: string;
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub state: TaskState,
    pub version: i64,
    /// Resumes can be an array of strings, a number, or a boolean.
    #[serde(default)]
    pub resumes: serde_json::Value,
    #[serde(default)]
    pub ttl: Option<i64>,
    #[serde(default)]
    pub pid: Option<String>,
}

/// A schedule record as returned by the server.
///
/// Mirrors the TS type:
/// ```ts
/// type ScheduleRecord = {
///   id: string; cron: string; promiseId: string;
///   promiseTimeout: number; promiseParam: Value;
///   promiseTags: Record<string, string>;
///   createdAt: number; nextRunAt: number; lastRunAt?: number;
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRecord {
    pub id: String,
    pub cron: String,
    pub promise_id: String,
    pub promise_timeout: i64,
    #[serde(default)]
    pub promise_param: Value,
    #[serde(default)]
    pub promise_tags: HashMap<String, String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub next_run_at: i64,
    #[serde(default)]
    pub last_run_at: Option<i64>,
}

// =============================================================================
// REQUEST TYPES
// =============================================================================

/// How to settle a durable promise.
///
/// Mirrors the TS type:
/// ```ts
/// state: "resolved" | "rejected" | "rejected_canceled"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettleState {
    #[serde(rename = "resolved")]
    Resolved,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "rejected_canceled")]
    RejectedCanceled,
}

/// Request to create a durable promise (`promise.create` data payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromiseCreateReq {
    pub id: String,
    pub timeout_at: i64,
    pub param: Value,
    // #[serde(default)]
    pub tags: HashMap<String, String>,
}

impl PromiseCreateReq {
    /// Create a minimal placeholder request (used when serialization fails at construction time).
    pub(crate) fn default_with_id(id: &str) -> Self {
        Self {
            id: id.to_string(),
            timeout_at: 0,
            param: Value {
                headers: None,
                data: None,
            },
            tags: HashMap::new(),
        }
    }
}

/// Request to settle a durable promise (`promise.settle` data payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromiseSettleReq {
    pub id: String,
    pub state: SettleState,
    pub value: Value,
}

/// A promise register callback request (`promise.register_callback` data payload).
///
/// Used inside `task.suspend` actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromiseRegisterCallbackData {
    pub awaited: String,
    pub awaiter: String,
}

// =============================================================================
// SDK-INTERNAL TYPES (not part of the wire protocol)
// =============================================================================

/// The result of executing a durable function.
#[derive(Debug)]
pub enum Outcome<T> {
    /// Function completed successfully or with an error.
    Done(crate::error::Result<T>),
    /// Function cannot proceed — it has unresolved remote dependencies.
    Suspended { remote_todos: Vec<String> },
}

/// The kind of durable function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableKind {
    /// Leaf function — no sub-tasks, always completes.
    Function,
    /// Workflow function — can call ctx.run/rpc, may suspend.
    Workflow,
}

/// Parsed task data from the root promise param.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskData {
    pub func: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

impl TaskData {
    /// Build a `Value` encoding `{"func": ..., "args": ...}` for remote dispatch.
    pub fn into_value<A: Serialize>(func: &str, args: A) -> crate::error::Result<Value> {
        Value::from_serializable(serde_json::json!({
            "func": func,
            "args": serde_json::to_value(args)?,
        }))
    }
}

/// Execution status returned from Core methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Done,
    Suspended,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Point {
        x: i64,
        y: i64,
    }

    // --- serialization: `skip_serializing_if = is_none` ---

    #[test]
    fn encode_empty_value_is_empty_object() {
        assert_eq!(serde_json::to_string(&Value::default()).unwrap(), "{}");
    }

    #[test]
    fn encode_headers_only() {
        let v = Value {
            headers: Some(HashMap::from([("a".to_string(), "b".to_string())])),
            data: None,
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"headers":{"a":"b"}}"#
        );
    }

    #[test]
    fn encode_data_only() {
        let v = Value {
            headers: None,
            data: Some(serde_json::json!(42)),
        };
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"data":42}"#);
    }

    #[test]
    fn encode_both_fields_in_field_order() {
        let v = Value {
            headers: Some(HashMap::from([("a".to_string(), "b".to_string())])),
            data: Some(serde_json::json!(42)),
        };
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"headers":{"a":"b"},"data":42}"#
        );
    }

    // --- data accessors ---

    #[test]
    fn data_or_null_defaults_to_null() {
        assert_eq!(Value::default().data_or_null(), serde_json::Value::Null);
    }

    #[test]
    fn data_or_null_returns_data() {
        let v = Value {
            headers: None,
            data: Some(serde_json::json!(42)),
        };
        assert_eq!(v.data_or_null(), serde_json::json!(42));
    }

    // --- headers_or_empty ---

    #[test]
    fn headers_or_empty_defaults_to_empty() {
        assert_eq!(Value::default().headers_or_empty(), HashMap::new());
    }

    #[test]
    fn headers_or_empty_returns_headers() {
        let headers = HashMap::from([("a".to_string(), "b".to_string())]);
        let v = Value {
            headers: Some(headers.clone()),
            data: None,
        };
        assert_eq!(v.headers_or_empty(), headers);
    }

    // --- from_serializable ---

    #[test]
    fn from_serializable_wraps_data() {
        let v = Value::from_serializable(Point { x: 1, y: 2 }).unwrap();
        assert!(v.headers.is_none());
        assert_eq!(v.data_or_null(), serde_json::json!({"x": 1, "y": 2}));
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"data":{"x":1,"y":2}}"#
        );
    }

    #[test]
    fn from_serializable_unserializable_errors() {
        // serde_json cannot use a non-string map key -> SerializationError.
        let map: HashMap<(i64, i64), i64> = HashMap::from([((1, 2), 3)]);
        let err = Value::from_serializable(map).unwrap_err();
        assert!(matches!(err, Error::SerializationError(_)));
    }

    // --- decode ---

    #[test]
    fn decode_roundtrip() {
        let v = Value::from_serializable(Point { x: 1, y: 2 }).unwrap();
        assert_eq!(v.decode::<Point>().unwrap(), Point { x: 1, y: 2 });
    }

    #[test]
    fn decode_null_into_required_type_errors() {
        let err = Value::default().decode::<i64>().unwrap_err();
        assert!(matches!(err, Error::SerializationError(_)));
    }

    // --- custom `Deserialize` impl ---

    #[test]
    fn from_wire_null_is_empty_value() {
        let v: Value = serde_json::from_str("null").unwrap();
        assert!(v.headers.is_none());
        assert!(v.data.is_none());
    }

    #[test]
    fn from_wire_object_splits_headers_and_data() {
        let v: Value = serde_json::from_str(r#"{"headers":{"a":"b"},"data":[1,2,3]}"#).unwrap();
        assert_eq!(
            v.headers_or_empty(),
            HashMap::from([("a".to_string(), "b".to_string())])
        );
        assert_eq!(v.data_or_null(), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn from_wire_invalid_headers_are_dropped() {
        // headers that are not a `str -> str` map become None.
        let v: Value = serde_json::from_str(r#"{"headers":[1,2],"data":1}"#).unwrap();
        assert!(v.headers.is_none());
        let v: Value = serde_json::from_str(r#"{"headers":{"a":1}}"#).unwrap();
        assert!(v.headers.is_none());
    }

    #[test]
    fn from_wire_bare_value_is_treated_as_data() {
        let v: Value = serde_json::from_str("42").unwrap();
        assert_eq!(v.data_or_null(), serde_json::json!(42));
        let v: Value = serde_json::from_str(r#""hello""#).unwrap();
        assert_eq!(v.data_or_null(), serde_json::json!("hello"));
        let v: Value = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(v.data_or_null(), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn from_wire_object_without_data_field() {
        let v: Value = serde_json::from_str("{}").unwrap();
        assert!(v.headers.is_none());
        assert_eq!(v.data_or_null(), serde_json::Value::Null);

        let v: Value = serde_json::from_str(r#"{"headers":{"a":"b"}}"#).unwrap();
        assert_eq!(
            v.headers_or_empty(),
            HashMap::from([("a".to_string(), "b".to_string())])
        );
        assert_eq!(v.data_or_null(), serde_json::Value::Null);
    }

    // --- PromiseRecord: camelCase wire format + `#[serde(default)]` parity ---

    const PROMISE_FULL: &str = concat!(
        r#"{"id":"p1","state":"resolved","#,
        r#""param":{"data":1},"#,
        r#""value":{"headers":{"a":"b"},"data":[1,2]},"#,
        r#""tags":{"k":"v"},"#,
        r#""timeoutAt":10,"createdAt":5,"settledAt":9}"#
    );

    #[test]
    fn promise_record_decode_full() {
        let r: PromiseRecord = serde_json::from_str(PROMISE_FULL).unwrap();
        assert_eq!(r.id, "p1");
        assert_eq!(r.state, PromiseState::Resolved);
        assert_eq!(r.param.data_or_null(), serde_json::json!(1));
        assert_eq!(
            r.value.headers_or_empty(),
            HashMap::from([("a".to_string(), "b".to_string())])
        );
        assert_eq!(r.value.data_or_null(), serde_json::json!([1, 2]));
        assert_eq!(r.tags, HashMap::from([("k".to_string(), "v".to_string())]));
        assert_eq!(r.timeout_at, 10);
        assert_eq!(r.created_at, 5);
        assert_eq!(r.settled_at, Some(9));
    }

    #[test]
    fn promise_record_decode_minimal_applies_defaults() {
        // Only the required fields; the rest come from `#[serde(default)]`.
        let r: PromiseRecord =
            serde_json::from_str(r#"{"id":"p1","state":"pending","timeoutAt":10}"#).unwrap();
        assert_eq!(r.param.data_or_null(), serde_json::Value::Null);
        assert!(r.param.headers.is_none());
        assert_eq!(r.value.data_or_null(), serde_json::Value::Null);
        assert!(r.tags.is_empty());
        assert_eq!(r.created_at, 0);
        assert_eq!(r.settled_at, None);
    }

    #[test]
    fn promise_record_decode_missing_required_field_errors() {
        assert!(serde_json::from_str::<PromiseRecord>(r#"{"id":"p1","state":"pending"}"#).is_err());
    }

    #[test]
    fn promise_record_encode_camel_and_field_order() {
        let r = PromiseRecord {
            id: "p1".to_string(),
            state: PromiseState::Pending,
            param: Value {
                headers: None,
                data: Some(serde_json::json!(1)),
            },
            value: Value::default(),
            tags: HashMap::from([("k".to_string(), "v".to_string())]),
            timeout_at: 10,
            created_at: 5,
            settled_at: None,
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            concat!(
                r#"{"id":"p1","state":"pending","param":{"data":1},"value":{},"#,
                r#""tags":{"k":"v"},"timeoutAt":10,"createdAt":5,"settledAt":null}"#
            )
        );
    }

    // --- TaskRecord ---

    #[test]
    fn task_record_decode_minimal_applies_defaults() {
        let r: TaskRecord =
            serde_json::from_str(r#"{"id":"t1","state":"pending","version":1}"#).unwrap();
        assert_eq!(r.resumes, serde_json::Value::Null);
        assert_eq!(r.ttl, None);
        assert_eq!(r.pid, None);
    }

    #[test]
    fn task_record_resumes_variants() {
        let cases = [
            (r#"["a","b"]"#, serde_json::json!(["a", "b"])),
            ("5", serde_json::json!(5)),
            ("true", serde_json::json!(true)),
            ("null", serde_json::Value::Null),
        ];
        for (raw, expected) in cases {
            let body = format!(r#"{{"id":"t","state":"pending","version":1,"resumes":{raw}}}"#);
            let r: TaskRecord = serde_json::from_str(&body).unwrap();
            assert_eq!(r.resumes, expected);
        }
    }

    #[test]
    fn task_record_encode() {
        let r = TaskRecord {
            id: "t1".to_string(),
            state: TaskState::Acquired,
            version: 2,
            resumes: serde_json::json!(["a"]),
            ttl: Some(30),
            pid: Some("x".to_string()),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":"t1","state":"acquired","version":2,"resumes":["a"],"ttl":30,"pid":"x"}"#
        );
    }

    #[test]
    fn task_record_encode_defaults_emit_null() {
        let r = TaskRecord {
            id: "t1".to_string(),
            state: TaskState::Pending,
            version: 1,
            resumes: serde_json::Value::Null,
            ttl: None,
            pid: None,
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":"t1","state":"pending","version":1,"resumes":null,"ttl":null,"pid":null}"#
        );
    }

    // --- ScheduleRecord ---

    const SCHEDULE_FULL: &str = concat!(
        r#"{"id":"s1","cron":"* * * * *","promiseId":"p1","promiseTimeout":100,"#,
        r#""promiseParam":{"data":1},"promiseTags":{"k":"v"},"#,
        r#""createdAt":1,"nextRunAt":2,"lastRunAt":3}"#
    );

    #[test]
    fn schedule_record_decode_full() {
        let r: ScheduleRecord = serde_json::from_str(SCHEDULE_FULL).unwrap();
        assert_eq!(r.id, "s1");
        assert_eq!(r.cron, "* * * * *");
        assert_eq!(r.promise_id, "p1");
        assert_eq!(r.promise_timeout, 100);
        assert_eq!(r.promise_param.data_or_null(), serde_json::json!(1));
        assert_eq!(
            r.promise_tags,
            HashMap::from([("k".to_string(), "v".to_string())])
        );
        assert_eq!(r.created_at, 1);
        assert_eq!(r.next_run_at, 2);
        assert_eq!(r.last_run_at, Some(3));
    }

    #[test]
    fn schedule_record_decode_minimal_applies_defaults() {
        let r: ScheduleRecord =
            serde_json::from_str(r#"{"id":"s1","cron":"c","promiseId":"p1","promiseTimeout":100}"#)
                .unwrap();
        assert_eq!(r.promise_param.data_or_null(), serde_json::Value::Null);
        assert!(r.promise_tags.is_empty());
        assert_eq!(r.created_at, 0);
        assert_eq!(r.next_run_at, 0);
        assert_eq!(r.last_run_at, None);
    }

    #[test]
    fn schedule_record_encode() {
        let r = ScheduleRecord {
            id: "s1".to_string(),
            cron: "c".to_string(),
            promise_id: "p1".to_string(),
            promise_timeout: 100,
            promise_param: Value::default(),
            promise_tags: HashMap::new(),
            created_at: 1,
            next_run_at: 2,
            last_run_at: None,
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            concat!(
                r#"{"id":"s1","cron":"c","promiseId":"p1","promiseTimeout":100,"#,
                r#""promiseParam":{},"promiseTags":{},"createdAt":1,"nextRunAt":2,"lastRunAt":null}"#
            )
        );
    }

    // --- PromiseCreateReq: camelCase wire format + default_with_id ---

    #[test]
    fn promise_create_req_encode_camel_and_field_order() {
        let r = PromiseCreateReq {
            id: "p1".to_string(),
            timeout_at: 10,
            param: Value {
                headers: None,
                data: Some(serde_json::json!(1)),
            },
            tags: HashMap::from([("k".to_string(), "v".to_string())]),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":"p1","timeoutAt":10,"param":{"data":1},"tags":{"k":"v"}}"#
        );
    }

    #[test]
    fn promise_create_req_decode_camel() {
        let r: PromiseCreateReq = serde_json::from_str(
            r#"{"id":"p1","timeoutAt":10,"param":{"data":1},"tags":{"k":"v"}}"#,
        )
        .unwrap();
        assert_eq!(r.id, "p1");
        assert_eq!(r.timeout_at, 10);
        assert_eq!(r.param.data_or_null(), serde_json::json!(1));
        assert_eq!(r.tags, HashMap::from([("k".to_string(), "v".to_string())]));
    }

    #[test]
    fn promise_create_req_default_with_id() {
        let r = PromiseCreateReq::default_with_id("p1");
        assert_eq!(r.id, "p1");
        assert_eq!(r.timeout_at, 0);
        assert!(r.param.headers.is_none());
        assert!(r.param.data.is_none());
        assert!(r.tags.is_empty());
    }

    // --- PromiseSettleReq ---

    #[test]
    fn promise_settle_req_encode() {
        let r = PromiseSettleReq {
            id: "p1".to_string(),
            state: SettleState::Resolved,
            value: Value {
                headers: None,
                data: Some(serde_json::json!(1)),
            },
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":"p1","state":"resolved","value":{"data":1}}"#
        );
    }

    #[test]
    fn promise_settle_req_decode() {
        let r: PromiseSettleReq =
            serde_json::from_str(r#"{"id":"p1","state":"rejected_canceled","value":{}}"#).unwrap();
        assert_eq!(r.id, "p1");
        assert_eq!(r.state, SettleState::RejectedCanceled);
        assert_eq!(r.value.data_or_null(), serde_json::Value::Null);
    }

    // --- PromiseRegisterCallbackData ---

    #[test]
    fn promise_register_callback_data_roundtrip() {
        let d = PromiseRegisterCallbackData {
            awaited: "a".to_string(),
            awaiter: "b".to_string(),
        };
        let encoded = serde_json::to_string(&d).unwrap();
        assert_eq!(encoded, r#"{"awaited":"a","awaiter":"b"}"#);
        let back: PromiseRegisterCallbackData = serde_json::from_str(&encoded).unwrap();
        assert_eq!(back.awaited, "a");
        assert_eq!(back.awaiter, "b");
    }

    // --- TaskData: `func` / `args` (no rename) + `args` default null + into_value ---

    #[test]
    fn task_data_decode_minimal_applies_default_args() {
        let d: TaskData = serde_json::from_str(r#"{"func":"f"}"#).unwrap();
        assert_eq!(d.func, "f");
        assert_eq!(d.args, serde_json::Value::Null);
    }

    #[test]
    fn task_data_decode_full() {
        let d: TaskData = serde_json::from_str(r#"{"func":"f","args":[1,2]}"#).unwrap();
        assert_eq!(d.func, "f");
        assert_eq!(d.args, serde_json::json!([1, 2]));
    }

    #[test]
    fn task_data_encode() {
        let d = TaskData {
            func: "f".to_string(),
            args: serde_json::json!({"x": 1}),
        };
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            r#"{"func":"f","args":{"x":1}}"#
        );
    }

    #[test]
    fn task_data_encode_default_args_emit_null() {
        let d = TaskData {
            func: "f".to_string(),
            args: serde_json::Value::Null,
        };
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            r#"{"func":"f","args":null}"#
        );
    }

    #[test]
    fn task_data_into_value_wraps_func_and_args() {
        let v = TaskData::into_value("f", serde_json::json!([1, 2])).unwrap();
        assert!(v.headers.is_none());
        // Compare structurally: serde_json (BTreeMap) and Python dict differ in
        // key order on the wire, but the decoded content is identical.
        assert_eq!(
            v.data_or_null(),
            serde_json::json!({"func": "f", "args": [1, 2]})
        );
    }

    #[test]
    fn task_data_into_value_unserializable_errors() {
        // serde_json cannot use a non-string map key -> SerializationError.
        let map: HashMap<(i64, i64), i64> = HashMap::from([((1, 2), 3)]);
        let err = TaskData::into_value("f", map).unwrap_err();
        assert!(matches!(err, Error::SerializationError(_)));
    }

    // --- DurableKind / Status: plain internal enums ---

    #[test]
    fn durable_kind_variants_distinct() {
        assert_ne!(DurableKind::Function, DurableKind::Workflow);
        let copied = DurableKind::Function;
        assert_eq!(copied, DurableKind::Function);
    }

    #[test]
    fn status_variants_distinct() {
        assert_ne!(Status::Done, Status::Suspended);
        let s = Status::Done;
        assert_eq!(s, Status::Done);
    }

    // --- Outcome: sum type `Outcome<T> { Done(Result<T>), Suspended }` ---

    #[test]
    fn outcome_done_holds_ok_result() {
        let o: Outcome<i64> = Outcome::Done(Ok(42));
        match o {
            Outcome::Done(Ok(v)) => assert_eq!(v, 42),
            _ => panic!("expected Done(Ok)"),
        }
    }

    #[test]
    fn outcome_done_holds_err_result() {
        let o: Outcome<i64> = Outcome::Done(Err(Error::Application {
            message: "boom".to_string(),
        }));
        match o {
            Outcome::Done(Err(Error::Application { message })) => {
                assert_eq!(message, "boom")
            }
            _ => panic!("expected Done(Err(Application))"),
        }
    }

    #[test]
    fn outcome_suspended_holds_remote_todos() {
        let o: Outcome<()> = Outcome::Suspended {
            remote_todos: vec!["a".to_string(), "b".to_string()],
        };
        match o {
            Outcome::Suspended { remote_todos } => {
                assert_eq!(remote_todos, vec!["a".to_string(), "b".to_string()])
            }
            _ => panic!("expected Suspended"),
        }
    }
}
