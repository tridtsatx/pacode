use serde_json::json;

use super::*;

#[test]
fn encode_round_trips_the_value() {
    let value = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
    let bytes = encode(value.clone()).expect("encode");
    let back: Value = serde_json::from_slice(&bytes).expect("parse");
    assert_eq!(back, value);
}

#[test]
fn body_keys_lists_top_level_keys() {
    let mut keys = body_keys(&json!({"a": 1, "b": 2}));
    keys.sort();
    assert_eq!(keys, vec!["a".to_string(), "b".to_string()]);
    assert!(body_keys(&json!([1])).is_empty());
}
