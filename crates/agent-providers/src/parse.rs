//! Reading a tool call out of whatever a small model actually wrote.
//!
//! This is the part of the provider layer that is not in the OpenAI spec. A 1.5B
//! model told to emit a tool call may answer with a fenced block, a bare object
//! in a sentence, a list of objects, an array under `tool_calls`, an XML-ish tag
//! from a ChatML chat template, or prose that describes the call in words. All of
//! those are handled here, and none of them has to fail the run: the loop gets the
//! calls it asked for, or an honest empty list it can send one repair for.

use agent_tools::ToolCall;
use serde_json::Value;

use crate::provider::ToolSchema;

/// Keys a model uses for the tool's name, checked in this order.
const NAME_KEYS: [&str; 6] = [
    "name",
    "tool",
    "tool_name",
    "function",
    "function_name",
    "call",
];

/// Keys a model uses for the arguments.
const ARG_KEYS: [&str; 7] = [
    "arguments",
    "args",
    "parameters",
    "params",
    "input",
    "payload",
    "values",
];

/// Keys whose value is a call, or a list of them, rather than a call itself.
const WRAPPER_KEYS: [&str; 6] = [
    "tool_calls",
    "tool_call",
    "calls",
    "functions",
    "function_call",
    "invocations",
];

/// The ChatML call tags some chat templates put in every assistant turn.
const CALL_TAG_HEAD: &str = "<tool_call";
const CLOSE_CALL_TAG: &str = "</tool_call>";

/// Every tool call this answer means, in the order the model wrote them.
///
/// `is_known` decides which names are tools. That filter is what stops a file
/// listing containing `{"name": "main.rs", "path": "src"}` from being read as a
/// call to a tool named `main.rs`, so a run passes its own tool names — see
/// [`tool_calls_among`].
///
/// An answer whose shape cannot be read yields nothing rather than a guess: an
/// agent that invents arguments to complete a half-parsed call does damage in a
/// real repository. The caller takes the "no calls here" path instead — one
/// repair attempt, then a plain answer.
pub fn tool_calls(text: &str, is_known: &dyn Fn(&str) -> bool) -> Vec<ToolCall> {
    let mut found = Vec::new();
    for candidate in candidates(text) {
        let Some(parsed) = parse_json(&candidate) else {
            continue;
        };
        let mut calls = Vec::new();
        collect(&parsed, is_known, &mut calls);
        found.extend(calls);
    }
    if found.is_empty() {
        for name in tagged_call_names(text) {
            if is_known(&name) {
                found.push(ToolCall::new(name, Value::Object(serde_json::Map::new())));
            }
        }
    }
    dedupe(found)
}

/// The calls in this answer whose names are among the tools the model was given.
pub fn tool_calls_among(text: &str, tools: &[ToolSchema]) -> Vec<ToolCall> {
    tool_calls(text, &|name| tools.iter().any(|tool| tool.name == name))
}

/// A JSON fragment's text, fixed enough to parse, or `None` if it cannot be.
///
/// The repairs are mechanical and none of them adds a key the model did not
/// write: close what the reply budget cut off, drop a comma before a closer,
/// turn a Python-style literal into a JSON one. No value is ever changed.
pub fn repair(json: &str) -> Option<String> {
    let trimmed = json.trim().trim_matches(';').trim();
    if trimmed.is_empty() {
        return None;
    }
    let opened = balance(trimmed)?;
    let substituted = substitute_literals(trimmed);
    let mut repaired = close_scope(&substituted, opened);
    if serde_json::from_str::<Value>(&repaired).is_err() {
        let without_commas = remove_trailing_commas(&repaired);
        if serde_json::from_str::<Value>(&without_commas).is_ok() {
            repaired = without_commas;
        }
    }
    serde_json::from_str::<Value>(&repaired)
        .is_ok()
        .then_some(repaired)
}

/// The fenced blocks if the answer has any, otherwise the JSON-shaped regions
/// buried in its prose.
fn candidates(text: &str) -> Vec<String> {
    let fenced = fenced_blocks(text);
    if fenced.is_empty() {
        return json_regions(text);
    }
    fenced
}

/// The bodies of ``` fences. An unterminated final fence is read as a reply cut
/// off at the end of the answer, which is what usually causes one.
fn fenced_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after_open = &rest[open + 3..];
        // A language tag (`json`) runs to the end of the fence's own line.
        let body = match after_open.find('\n') {
            Some(newline) => &after_open[newline + 1..],
            None => after_open,
        };
        match body.find("```") {
            Some(end) => {
                blocks.push(body[..end].to_owned());
                rest = &body[end + 3..];
            }
            None => {
                blocks.push(body.to_owned());
                return blocks;
            }
        }
    }
    blocks
}

/// Top-level balanced `{...}` and `[...]` regions, ignoring braces inside JSON
/// strings so a brace in a code example does not open a candidate.
fn json_regions(text: &str) -> Vec<String> {
    let mut regions = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'\\' if in_string => escaped = !escaped,
            b'"' if !escaped => in_string = !in_string,
            b'{' | b'[' if !in_string => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' | b']' if !in_string && depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(from) = start.take() {
                        regions.push(text[from..=index].to_owned());
                    }
                }
            }
            _ => {}
        }
        if byte != b'\\' {
            escaped = false;
        }
    }
    // An answer truncated by the reply budget leaves its last region open.
    if let Some(from) = start {
        regions.push(text[from..].to_owned());
    }
    regions
}

/// Parse directly, then parse again after repair.
fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .or_else(|| repair(text).and_then(|fixed| serde_json::from_str::<Value>(&fixed).ok()))
}

/// Which brackets the fragment leaves open, or `None` if it closes something that
/// was never opened or is nested past the point of a sensible fix.
fn balance(text: &str) -> Option<Vec<u8>> {
    let mut stack: Vec<u8> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        match byte {
            b'\\' if in_string => escaped = !escaped,
            b'"' if !escaped => in_string = !in_string,
            _ if in_string => {}
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                let opener = if byte == b'}' { b'{' } else { b'[' };
                if stack.pop() != Some(opener) {
                    return None;
                }
            }
            _ => {}
        }
    }
    (stack.len() <= 8).then_some(stack)
}

/// Close an unterminated string first, then any open brackets, innermost last.
fn close_scope(text: &str, opened: Vec<u8>) -> String {
    let mut out = text.to_owned();
    if ends_inside_string(&out) {
        out.push('"');
    }
    for opener in opened.iter().rev() {
        out.push(if *opener == b'{' { '}' } else { ']' });
    }
    out
}

/// Whether the fragment stops mid-string — the usual shape of a reply cut off
/// mid-answer.
fn ends_inside_string(text: &str) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        match byte {
            b'\\' if in_string => escaped = !escaped,
            b'"' if !escaped => in_string = !in_string,
            _ => {}
        }
    }
    in_string
}

/// `{"a": 1,}` and `[{"x": 1,},]` parse once the comma before a closer is gone.
fn remove_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut lookahead = chars.clone();
            while matches!(lookahead.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// `True`, `False` and `None` are what a model trained on Python writes for
/// `true`, `false` and `null`. Only outside strings, and only whole words.
fn substitute_literals(text: &str) -> String {
    const LITERALS: [(&str, &str); 6] = [
        ("False", "false"),
        ("True", "true"),
        ("None", "null"),
        ("NULL", "null"),
        ("TRUE", "true"),
        ("FALSE", "false"),
    ];
    let is_word_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < text.len() {
        let byte = text.as_bytes()[index];
        if in_string {
            out.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            out.push('"');
            index += 1;
            continue;
        }
        let mut replaced = None;
        for (from, to) in LITERALS {
            let Some(rest) = text[index..].strip_prefix(from) else {
                continue;
            };
            // Both sides must be word boundaries, or `NewTrue` becomes `Newtrue`.
            let after_ok = !rest.bytes().next().is_some_and(is_word_byte);
            let before_ok = index == 0 || !is_word_byte(text.as_bytes()[index - 1]);
            if after_ok && before_ok {
                replaced = Some((to, from.len()));
                break;
            }
        }
        match replaced {
            Some((to, len)) => {
                out.push_str(to);
                index += len;
            }
            None => {
                out.push(byte as char);
                index += 1;
            }
        }
    }
    out
}

/// A value in any of the shapes a call can appear in, gathered so the order
/// matches the model's own.
fn collect(value: &Value, is_known: &dyn Fn(&str) -> bool, calls: &mut Vec<ToolCall>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, is_known, calls);
            }
        }
        Value::Object(map) => {
            if let Some(call) = call_from_object(value, is_known) {
                calls.push(call);
                return;
            }
            for key in WRAPPER_KEYS {
                if let Some(inner) = map.get(key) {
                    collect(inner, is_known, calls);
                }
            }
        }
        _ => {}
    }
}

/// One call out of an object, in any of the key spellings and nestings models
/// use. `None` unless the object names a tool this run actually has.
fn call_from_object(value: &Value, is_known: &dyn Fn(&str) -> bool) -> Option<ToolCall> {
    let name = name_in(value)?;
    if !is_known(&name) {
        return None;
    }
    let arguments = match arguments_in(value) {
        Arguments::Read(object) => object,
        // No argument field anywhere: take the keys beside the name instead.
        Arguments::Absent => flat_arguments(value),
        // An `args` field the model meant but that is not an object. Refusing
        // here is the whole point of this module's contract: no call is better
        // than a call whose arguments were invented by the parser.
        Arguments::Unreadable => return None,
    };
    Some(ToolCall::new(name, arguments))
}

/// Where an object's arguments are, and whether they can be read at all.
enum Arguments {
    Read(Value),
    Absent,
    Unreadable,
}

fn name_in(value: &Value) -> Option<String> {
    for key in NAME_KEYS {
        let Some(field) = value.get(key) else {
            continue;
        };
        if let Some(text) = field.as_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
            continue;
        }
        // {"function": {"name": "read_file", …}}
        if let Some(nested) = name_in(field) {
            return Some(nested);
        }
    }
    None
}

fn arguments_in(value: &Value) -> Arguments {
    let mut unreadable = false;
    for key in ARG_KEYS {
        let Some(field) = value.get(key) else {
            continue;
        };
        match field {
            Value::Object(_) => return Arguments::Read(field.clone()),
            Value::String(text) => match parse_json(text.trim()).filter(Value::is_object) {
                // An OpenAI-style `arguments` is a JSON string.
                Some(object) => return Arguments::Read(object),
                // A bare word where an object belongs. Keep looking in case a
                // later key is readable, refuse the call if none is.
                None => unreadable = true,
            },
            _ => return Arguments::Unreadable,
        }
    }
    for key in ["function", "tool_call", "call", "invoke"] {
        if let Some(inner) = value.get(key) {
            let found = arguments_in(inner);
            if !matches!(found, Arguments::Absent) {
                return found;
            }
        }
    }
    if unreadable {
        return Arguments::Unreadable;
    }
    Arguments::Absent
}

/// The flat shape: the tool's argument keys written beside its name, which is
/// what most small models do because the wrapper costs tokens to remember.
fn flat_arguments(value: &Value) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(object) = value.as_object() {
        for (key, field) in object {
            if NAME_KEYS.contains(&key.as_str()) || ARG_KEYS.contains(&key.as_str()) {
                continue;
            }
            map.insert(key.clone(), field.clone());
        }
    }
    Value::Object(map)
}

/// Names from the ChatML call tags some chat templates emit, in both
/// spellings: <tool_call> with a JSON body inside, and <tool_call:name> with the
/// name in the header. Only these two are recognised — matching every
/// angle-bracketed word would read an HTML tag in a diff as a tool call.
fn tagged_call_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(CALL_TAG_HEAD) {
        let open = cursor + relative;
        let Some(header_offset) = text[open..].find('>') else {
            break;
        };
        let header_end = open + header_offset + 1;
        let header = text[open + CALL_TAG_HEAD.len()..header_end - 1]
            .trim_start_matches([':', ' '])
            .trim();
        let body_start = header_end + usize::from(text[header_end..].starts_with('\n'));
        let body_end = text[body_start..]
            .find(CLOSE_CALL_TAG)
            .map(|relative| body_start + relative)
            .unwrap_or(text.len());
        let mut calls = Vec::new();
        if let Some(parsed) = parse_json(&text[body_start..body_end]) {
            collect(&parsed, &|_| true, &mut calls);
        }
        if let Some(call) = calls.into_iter().next() {
            names.push(call.name);
        } else if looks_like_tool_name(header) {
            names.push(header.to_owned());
        }
        cursor = body_end;
    }
    names
}

/// A tag header is a tool name only if it is spelled like one: letters, digits
/// and `_`, `-`, `.`.
fn looks_like_tool_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// Drop repeats: a model that restates its call in prose after the fence would
/// otherwise run the tool twice.
fn dedupe(calls: Vec<ToolCall>) -> Vec<ToolCall> {
    let mut out: Vec<ToolCall> = Vec::with_capacity(calls.len());
    for call in calls {
        if !out.contains(&call) {
            out.push(call);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOOLS: [&str; 5] = [
        "read_file",
        "list_files",
        "run_command",
        "edit_file",
        "repo_map",
    ];

    fn known(text: &str) -> Vec<ToolCall> {
        tool_calls(text, &|name| TOOLS.contains(&name))
    }

    fn names(calls: &[ToolCall]) -> Vec<&str> {
        calls.iter().map(|call| call.name.as_str()).collect()
    }

    #[test]
    fn a_fenced_block_is_the_common_case() {
        let text = "I will read the file first.\n```json\n{ \"tool\": \"read_file\", \"path\": \"src/main.rs\" }\n```\nThen I will decide.";
        let calls = known(text);
        assert_eq!(names(&calls), ["read_file"]);
        assert_eq!(calls[0].arguments["path"], "src/main.rs");
    }

    #[test]
    fn a_bare_object_in_a_sentence_is_found_too() {
        let calls = known(r#"Sure — {"name": "list_files", "args": {"dir": "src"}} one moment."#);
        assert_eq!(names(&calls), ["list_files"]);
        assert_eq!(calls[0].arguments["dir"], "src");
    }

    #[test]
    fn every_argument_key_spelling_works() {
        for key in ARG_KEYS {
            let text = format!(r#"{{"name": "edit_file", "{key}": {{"path": "a.rs"}}}}"#);
            let calls = known(&text);
            assert_eq!(names(&calls), ["edit_file"], "{key} was not read");
            assert_eq!(calls[0].arguments["path"], "a.rs");
        }
    }

    #[test]
    fn arguments_written_as_a_json_string_are_parsed() {
        let value = json!({
            "tool_calls": [
                {"function": {"name": "run_command", "arguments": "{\"command\": \"cargo test\"}"}}
            ]
        });
        let calls = known(&value.to_string());
        assert_eq!(names(&calls), ["run_command"]);
        assert_eq!(calls[0].arguments["command"], "cargo test");
    }

    #[test]
    fn a_call_with_no_arguments_is_still_a_call() {
        let calls = known("```\n{\"name\": \"repo_map\"}\n```");
        assert_eq!(names(&calls), ["repo_map"]);
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn several_calls_in_one_answer_keep_their_order() {
        let text = "```json\n[{\"tool\": \"read_file\", \"path\": \"a\"}, {\"tool\": \"run_command\", \"command\": \"ls\"}]\n```";
        assert_eq!(names(&known(text)), ["read_file", "run_command"]);
    }

    #[test]
    fn a_chatml_tagged_call_survives_its_prose() {
        let open = format!("{CALL_TAG_HEAD}>");
        let text = format!(
            "Looking now.\n{open}\n{{\"tool\": \"list_files\", \"args\": {{\"dir\": \".\"}}}}\n{close}",
            close = CLOSE_CALL_TAG,
        );
        let calls = known(&text);
        assert_eq!(names(&calls), ["list_files"]);
        assert_eq!(calls[0].arguments["dir"], ".");
    }

    #[test]
    fn a_named_tag_with_an_unreadable_body_uses_the_tag_name() {
        let named = format!("{CALL_TAG_HEAD}:edit_file>");
        let text = format!(
            "{named}\nI could not write the JSON\n{close}",
            close = CLOSE_CALL_TAG
        );
        assert_eq!(names(&known(&text)), ["edit_file"]);
    }

    #[test]
    fn a_name_that_is_not_a_tool_is_not_a_call() {
        // The false positive the filter exists for: a file record, not a request.
        assert!(known(r#"{"files": [{"name": "main.rs", "path": "src/main.rs"}]}"#).is_empty());
        assert!(known(r#"{"tool": "delete_everything", "path": "."}"#).is_empty());
    }

    #[test]
    fn arguments_that_are_not_an_object_are_refused_not_invented() {
        assert!(known(r#"{"tool": "read_file", "args": ["src/main.rs"]}"#).is_empty());
        assert!(known(r#"{"tool": "read_file", "args": "src/main.rs"}"#).is_empty());
    }

    #[test]
    fn a_fenced_block_wins_over_braces_elsewhere_in_the_answer() {
        let text =
            "Here is the pattern: { not json at all }\n```json\n{\"tool\": \"repo_map\"}\n```";
        assert_eq!(names(&known(text)), ["repo_map"]);
    }

    #[test]
    fn a_brace_inside_a_string_does_not_open_a_region() {
        let calls = known(r#"{"name": "edit_file", "args": {"text": "if x { "}}}"#);
        assert_eq!(names(&calls), ["edit_file"]);
    }

    #[test]
    fn prose_that_asks_for_nothing_yields_nothing() {
        assert!(known("The bug is in the parser; I would add a test there.").is_empty());
    }

    #[test]
    fn a_repeated_call_is_only_made_once() {
        let text = "```json\n{\"tool\": \"repo_map\"}\n```\nAs said: {\"tool\": \"repo_map\"}";
        assert_eq!(names(&known(text)), ["repo_map"]);
    }

    #[test]
    fn a_truncated_object_is_closed_and_read() {
        let broken = r#"{"tool": "read_file", "path": "src/main"#;
        let fixed = repair(broken).unwrap_or_else(|| panic!("{broken} was not repaired"));
        assert_eq!(
            serde_json::from_str::<Value>(&fixed).unwrap()["path"],
            "src/main"
        );
    }

    #[test]
    fn a_trailing_comma_is_dropped() {
        let fixed = repair(r#"{"tool": "repo_map", "args": {},}"#).expect("repaired");
        assert_eq!(names(&known(&fixed)), ["repo_map"]);
    }

    #[test]
    fn python_literals_become_json_ones() {
        let fixed = repair(r#"{"tool": "run_command", "args": {"dry_run": True, "seed": None}}"#)
            .expect("repaired");
        let value: Value = serde_json::from_str(&fixed).expect("parses");
        assert_eq!(value["args"]["dry_run"], json!(true));
        assert_eq!(value["args"]["seed"], json!(null));
    }

    #[test]
    fn a_literal_word_inside_a_string_is_left_alone() {
        let text = r#"{"tool": "edit_file", "args": {"text": "True is a word"}}"#;
        assert_eq!(substitute_literals(text), text);
    }

    #[test]
    fn nonsense_is_not_repaired_into_something() {
        assert_eq!(repair("not json"), None);
        assert_eq!(repair(""), None);
        assert_eq!(repair(r#"{"a": }"#), None);
    }
}
