//! JSON Schema, written once so every tool's arguments look the same to a
//! model that has already seen one.
//!
//! These builders exist because a hand-written `json!` object per tool drifts:
//! one tool says `path`, another `file`, one describes what a field is, another
//! what to put in it. With small models the description string is the only
//! documentation they get, so it is worth saying what a value is *for* and what
//! happens when it is wrong.

use serde_json::{json, Value};

/// An object with these properties, and these of them required.
pub fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

pub fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

/// A string that names a path in the repository, relative to its root.
pub fn path(description: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("{description} Relative to the repository root; a path outside \
                               it is refused."),
    })
}

pub fn integer(description: &str, minimum: i64) -> Value {
    json!({ "type": "integer", "minimum": minimum, "description": description })
}

pub fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

pub fn string_array(description: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_says_which_fields_it_needs() {
        let schema = object(json!({ "path": path("the file to read") }), &["path"]);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["additionalProperties"], false);
        let description = schema["properties"]["path"]["description"]
            .as_str()
            .expect("a description");
        assert!(description.contains("refused"), "{description}");
    }

    #[test]
    fn a_number_carries_its_floor() {
        let schema = integer("line to start at, from 1", 1);
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["minimum"], 1);
    }
}
