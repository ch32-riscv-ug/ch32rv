//! en: Checks a `--json` envelope or an NDJSON event against docs/contract/*.schema.json
//! (docs/contract/README.ja.md rule 5), for every test that reads ch32rv's JSON. The validator
//! covers the keywords these schemas use (type, required, properties, additionalProperties as a
//! schema, const, enum, items, if / then, allOf, minimum, $ref to a local `#/$defs/...` or to a
//! sibling file); a keyword it does not know fails the test, so a schema that grows one cannot be
//! checked by mistake. `additionalProperties: false` is refused: a reader ignores unknown fields.
//! ja: `--json` の封筒と NDJSON の event を docs/contract の schema と照合する(JSON を読む試験すべてで
//! 使う)。照合は schema が使う keyword だけを扱い、知らない keyword があれば試験を落とす。
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;

pub fn root(rel: &str) -> String {
    format!("{}/../{rel}", env!("CARGO_MANIFEST_DIR"))
}

pub fn schema(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root(&format!("docs/contract/{name}"))).unwrap())
        .unwrap()
}

/// The contract version every envelope carries (`ch32rv_contract::CONTRACT_VERSION`).
pub const CONTRACT: &str = "5";

const KNOWN: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "$ref",
    "title",
    "description",
    "type",
    "required",
    "properties",
    "additionalProperties",
    "const",
    "enum",
    "items",
    "if",
    "then",
    "allOf",
    "minimum",
];

/// Errors of `v` against `s` (a part of the schema file `doc`) at `path`; empty = valid.
pub fn check_in(doc: &Value, s: &Value, v: &Value, path: &str) -> Vec<String> {
    let Some(obj) = s.as_object() else {
        return Vec::new();
    };
    let mut errs = Vec::new();
    for k in obj.keys() {
        assert!(
            KNOWN.contains(&k.as_str()),
            "schema keyword `{k}` at {path} is not checked here"
        );
    }
    if let Some(Value::String(r)) = obj.get("$ref") {
        let (file, pointer) = r.split_once('#').unwrap_or((r.as_str(), ""));
        let other;
        let target_doc = if file.is_empty() {
            doc
        } else {
            other = schema(file);
            &other
        };
        let target = target_doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("unresolved $ref {r} at {path}"));
        errs.extend(check_in(target_doc, target, v, path));
    }
    if let Some(t) = obj.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let ok = types.iter().any(|t| match *t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => false,
        });
        if !ok {
            errs.push(format!("{path}: {v} is not {types:?}"));
            return errs;
        }
    }
    if let Some(c) = obj.get("const")
        && v != c
    {
        errs.push(format!("{path}: {v} is not the const {c}"));
    }
    if let Some(Value::Array(e)) = obj.get("enum")
        && !e.contains(v)
    {
        errs.push(format!("{path}: {v} is not one of {e:?}"));
    }
    if let (Some(m), Some(n)) = (obj.get("minimum").and_then(Value::as_f64), v.as_f64())
        && n < m
    {
        errs.push(format!("{path}: {n} < minimum {m}"));
    }
    if let (Some(Value::Array(req)), Some(o)) = (obj.get("required"), v.as_object()) {
        for r in req.iter().filter_map(Value::as_str) {
            if !o.contains_key(r) {
                errs.push(format!("{path}: missing `{r}`"));
            }
        }
    }
    let props = obj.get("properties").and_then(Value::as_object);
    if let (Some(props), Some(o)) = (props, v.as_object()) {
        for (k, ps) in props {
            if let Some(x) = o.get(k) {
                errs.extend(check_in(doc, ps, x, &format!("{path}.{k}")));
            }
        }
    }
    if let Some(extra) = obj.get("additionalProperties") {
        assert!(
            extra.is_object(),
            "additionalProperties at {path} must be a schema: readers ignore unknown fields"
        );
        if let Some(o) = v.as_object() {
            for (k, x) in o {
                if !props.is_some_and(|p| p.contains_key(k)) {
                    errs.extend(check_in(doc, extra, x, &format!("{path}.{k}")));
                }
            }
        }
    }
    if let (Some(items), Some(a)) = (obj.get("items"), v.as_array()) {
        for (i, x) in a.iter().enumerate() {
            errs.extend(check_in(doc, items, x, &format!("{path}[{i}]")));
        }
    }
    if let Some(cond) = obj.get("if")
        && check_in(doc, cond, v, path).is_empty()
        && let Some(then) = obj.get("then")
    {
        errs.extend(check_in(doc, then, v, path));
    }
    if let Some(Value::Array(all)) = obj.get("allOf") {
        for a in all {
            errs.extend(check_in(doc, a, v, path));
        }
    }
    errs
}

/// Errors of `v` against the whole schema file `name`.
pub fn check(name: &str, v: &Value) -> Vec<String> {
    let doc = schema(name);
    check_in(&doc, &doc, v, "$")
}

/// `v` is a valid envelope of the current contract (its `result` checked by `cmd`).
pub fn assert_envelope(v: &Value) {
    let errs = check("result.schema.json", v);
    assert!(errs.is_empty(), "{}\n{v}", errs.join("\n"));
    assert_eq!(v["contract"], CONTRACT, "{v}");
}

/// `v` is a valid NDJSON event.
pub fn assert_event(v: &Value) {
    let errs = check("events.schema.json", v);
    assert!(errs.is_empty(), "{}\n{v}", errs.join("\n"));
}

/// The envelope on `stdout` (one JSON object), checked against the contract.
pub fn envelope(stdout: &[u8]) -> Value {
    let v: Value = serde_json::from_slice(stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(stdout)));
    assert_envelope(&v);
    v
}
