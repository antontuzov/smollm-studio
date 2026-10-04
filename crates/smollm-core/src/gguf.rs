//! Minimal, dependency-free GGUF metadata reader.
//!
//! Supports the GGUF v2/v3 header: key/value metadata plus tensor-info
//! parsing for a parameter count. Tensor *data* is never read, so probing a
//! multi-gigabyte file stays cheap.

use std::io::{BufReader, Cursor, Read};
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};
use crate::model::ModelMetadata;

const MAGIC: &[u8; 4] = b"GGUF";
const MAX_KV_PAIRS: u64 = 200_000;
const MAX_TENSORS: u64 = 200_000;
const MAX_STRING_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARRAY_ENTRIES: u64 = 260_000;
const MAX_TENSOR_RANK: u32 = 8;
/// Vocabulary arrays are huge and useless in the UI; they collapse to a marker.
const MAX_STORED_ARRAY_ITEMS: usize = 512;

/// Metadata value type tags from the GGUF spec.
mod type_id {
    pub const UINT8: u32 = 0;
    pub const INT8: u32 = 1;
    pub const UINT16: u32 = 2;
    pub const INT16: u32 = 3;
    pub const UINT32: u32 = 4;
    pub const INT32: u32 = 5;
    pub const FLOAT32: u32 = 6;
    pub const BOOL: u32 = 7;
    pub const STRING: u32 = 8;
    pub const ARRAY: u32 = 9;
    pub const UINT64: u32 = 10;
    pub const INT64: u32 = 11;
    pub const FLOAT64: u32 = 12;
}

/// `general.file_type` values as written by llama.cpp conversion scripts.
const FILE_TYPES: [&str; 18] = [
    "F32", "F16", "BF16", "Q4_0", "Q4_1", "Q5_0", "Q5_1", "Q8_0", "Q2_K", "Q3_K_S", "Q3_K_M",
    "Q3_K_L", "Q4_K_S", "Q4_K_M", "Q5_K_S", "Q5_K_M", "Q6_K", "IQ4_NL",
];

/// A parsed GGUF header.
#[derive(Debug, Clone)]
pub struct GgufHeader {
    pub version: u32,
    pub n_tensors: u64,
    pub metadata: Map<String, Value>,
    /// Sum of tensor element counts, when tensor infos were readable.
    pub total_params: Option<u64>,
}

impl GgufHeader {
    /// Read header + metadata + tensor infos from a file on disk.
    pub fn read(path: &Path) -> AppResult<Self> {
        let file = std::fs::File::open(path).map_err(|source| {
            AppError::GgufParse(format!("cannot open {}: {source}", path.display()))
        })?;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        Self::parse(&mut reader)
    }

    /// Parse from an in-memory buffer (tests, fixtures, streamed prefixes).
    pub fn parse_bytes(bytes: &[u8]) -> AppResult<Self> {
        Self::parse(&mut Cursor::new(bytes))
    }

    pub fn parse<R: Read>(reader: &mut R) -> AppResult<Self> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(AppError::GgufParse(format!(
                "bad magic {:?}, expected GGUF",
                String::from_utf8_lossy(&magic)
            )));
        }

        let version = read_u32(reader)?;
        if !(2..=3).contains(&version) {
            return Err(AppError::GgufParse(format!(
                "unsupported GGUF version {version} (expected 2 or 3)"
            )));
        }

        let n_tensors = read_u64(reader)?;
        let n_kv = read_u64(reader)?;
        if n_kv > MAX_KV_PAIRS {
            return Err(AppError::GgufParse(format!(
                "implausible metadata count {n_kv}"
            )));
        }

        let mut metadata = Map::with_capacity(n_kv.min(64) as usize);
        for _ in 0..n_kv {
            let key = read_string(reader)?;
            metadata.insert(key, read_value(reader, 0)?);
        }

        // Tensor infos are best effort: some quantised exports pad before them,
        // and metadata is still worth showing even if we cannot count params.
        let total_params = if n_tensors == 0 || n_tensors > MAX_TENSORS {
            None
        } else {
            read_tensor_params(reader, n_tensors, version).unwrap_or(None)
        };

        Ok(Self {
            version,
            n_tensors,
            metadata,
            total_params,
        })
    }

    /// Collapse raw key/values into the app's model metadata view.
    pub fn summary(&self) -> ModelMetadata {
        let architecture = self.string("general.architecture");
        let prefix = architecture.clone().unwrap_or_else(|| "llama".to_string());

        let quantization = self
            .metadata
            .get("general.file_type")
            .and_then(Value::as_u64)
            .and_then(|index| FILE_TYPES.get(index as usize).copied())
            .map(str::to_string);

        let parameter_count = self.total_params.or_else(|| {
            self.metadata
                .get("general.parameter_count")
                .and_then(Value::as_u64)
        });

        ModelMetadata {
            name: self.string("general.name"),
            architecture,
            quantization,
            parameter_count,
            parameters_b: parameter_count.map(to_billions),
            context_length: self.u32(&format!("{prefix}.context_length")),
            train_type: self.string("general.type"),
            license: self.string("general.license"),
            vocab_size: self
                .metadata
                .get("tokenizer.ggml.n_vocab")
                .and_then(Value::as_u64),
            block_count: self.u32(&format!("{prefix}.block_count")),
            embedding_length: self.u32(&format!("{prefix}.embedding_length")),
            n_tensors: self.n_tensors,
            gguf_version: self.version,
        }
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.metadata
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn u32(&self, key: &str) -> Option<u32> {
        self.metadata
            .get(key)
            .and_then(Value::as_u64)
            .map(|v| v as u32)
    }

    /// True when the file carries a chat template or is typed as instruct.
    pub fn is_chat_model(&self) -> bool {
        self.metadata.contains_key("tokenizer.chat_template")
            || self
                .string("general.type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("instruct"))
    }
}

fn to_billions(parameters: u64) -> f32 {
    // Round to 0.1B so 1,720,000,000 reports as 1.7 rather than 1.7200001.
    (parameters as f64 / 1e8).round() as f32 / 10.0
}

fn read_tensor_params<R: Read>(
    reader: &mut R,
    n_tensors: u64,
    version: u32,
) -> AppResult<Option<u64>> {
    // GGUF v3 narrowed tensor dimensions to uint32; v2 wrote uint64.
    let mut total: u64 = 0;
    for _ in 0..n_tensors {
        let _name = read_string(reader)?;
        let n_dims = read_u32(reader)?;
        if n_dims > MAX_TENSOR_RANK {
            return Err(AppError::GgufParse(format!(
                "implausible tensor rank {n_dims}"
            )));
        }
        let mut elements: u64 = 1;
        for _ in 0..n_dims {
            let dim = if version >= 3 {
                u64::from(read_u32(reader)?)
            } else {
                read_u64(reader)?
            };
            elements = elements.saturating_mul(dim);
        }
        let _dtype = read_u32(reader)?;
        let _offset = read_u64(reader)?;
        total = total.saturating_add(elements);
    }
    Ok(Some(total))
}

/// Read a metadata value, including its leading type byte.
fn read_value<R: Read>(reader: &mut R, depth: u8) -> AppResult<Value> {
    if depth > 4 {
        return Err(AppError::GgufParse("array nesting too deep".into()));
    }
    let kind = read_u32(reader)?;
    match kind {
        type_id::ARRAY => read_array_value(reader, depth),
        _ => read_element(reader, kind, depth),
    }
}

/// Read one array element (no leading type byte).
fn read_element<R: Read>(reader: &mut R, kind: u32, depth: u8) -> AppResult<Value> {
    Ok(match kind {
        type_id::UINT8 => Value::from(read_u8(reader)? as u64),
        type_id::INT8 => Value::from(read_i8(reader)? as i64),
        type_id::UINT16 => Value::from(read_u16(reader)? as u64),
        type_id::INT16 => Value::from(read_i16(reader)? as i64),
        type_id::UINT32 => Value::from(read_u32(reader)? as u64),
        type_id::INT32 => Value::from(read_i32(reader)? as i64),
        type_id::FLOAT32 => Value::from(read_f32(reader)?),
        type_id::BOOL => Value::Bool(read_u8(reader)? != 0),
        type_id::STRING => Value::String(read_string(reader)?),
        type_id::UINT64 => Value::from(read_u64(reader)?),
        type_id::INT64 => Value::from(read_i64(reader)?),
        type_id::FLOAT64 => Value::from(read_f64(reader)?),
        type_id::ARRAY => {
            if depth > 4 {
                return Err(AppError::GgufParse("array nesting too deep".into()));
            }
            read_array_value(reader, depth)?
        }
        other => {
            return Err(AppError::GgufParse(format!(
                "unknown metadata type {other}"
            )))
        }
    })
}

fn read_array_value<R: Read>(reader: &mut R, depth: u8) -> AppResult<Value> {
    let element_kind = read_u32(reader)?;
    let count = read_u64(reader)?;
    if count > MAX_ARRAY_ENTRIES {
        return Err(AppError::GgufParse(format!(
            "implausible array length {count}"
        )));
    }

    let mut items = Vec::with_capacity(count.min(MAX_STORED_ARRAY_ITEMS as u64) as usize);
    let mut last_sample: Option<Value> = None;
    for _ in 0..count {
        let value = read_element(reader, element_kind, depth)?;
        if items.len() < MAX_STORED_ARRAY_ITEMS {
            items.push(value.clone());
        }
        last_sample = Some(value);
    }

    if count as usize > items.len() {
        // Keep the shape recognisable without shipping a full vocabulary to the UI.
        let kind_label = match element_kind {
            type_id::STRING => "strings",
            _ => "values",
        };
        let sample = last_sample.map_or_else(Vec::new, |value| vec![value]);
        let mut object = Map::new();
        object.insert("truncated".to_string(), Value::Bool(true));
        object.insert("count".to_string(), Value::from(count));
        object.insert("kind".to_string(), Value::String(kind_label.to_string()));
        object.insert("sample".to_string(), Value::Array(sample));
        return Ok(Value::Object(object));
    }

    Ok(Value::Array(items))
}

fn read_string<R: Read>(reader: &mut R) -> AppResult<String> {
    let len = read_u64(reader)?;
    if len > MAX_STRING_BYTES {
        return Err(AppError::GgufParse(format!("string too long: {len} bytes")));
    }
    let mut buffer = vec![0u8; len as usize];
    reader.read_exact(&mut buffer)?;
    String::from_utf8(buffer)
        .map_err(|source| AppError::GgufParse(format!("invalid utf8: {source}")))
}

macro_rules! read_scalar {
    ($name:ident, $ty:ty, $size:expr) => {
        fn $name<R: Read>(reader: &mut R) -> AppResult<$ty> {
            let mut buffer = [0u8; $size];
            reader.read_exact(&mut buffer)?;
            Ok(<$ty>::from_le_bytes(buffer))
        }
    };
}

read_scalar!(read_u16, u16, 2);
read_scalar!(read_i16, i16, 2);
read_scalar!(read_u32, u32, 4);
read_scalar!(read_i32, i32, 4);
read_scalar!(read_f32, f32, 4);
read_scalar!(read_u64, u64, 8);
read_scalar!(read_i64, i64, 8);
read_scalar!(read_f64, f64, 8);

fn read_u8<R: Read>(reader: &mut R) -> AppResult<u8> {
    let mut buffer = [0u8; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0])
}

fn read_i8<R: Read>(reader: &mut R) -> AppResult<i8> {
    let mut buffer = [0u8; 1];
    reader.read_exact(&mut buffer)?;
    Ok(buffer[0] as i8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_string(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(&(value.len() as u64).to_le_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn push_kv_string(out: &mut Vec<u8>, key: &str, value: &str) {
        push_string(out, key);
        out.extend_from_slice(&type_id::STRING.to_le_bytes());
        push_string(out, value);
    }

    fn push_kv_u32(out: &mut Vec<u8>, key: &str, value: u32) {
        push_string(out, key);
        out.extend_from_slice(&type_id::UINT32.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_kv_bool(out: &mut Vec<u8>, key: &str, value: bool) {
        push_string(out, key);
        out.extend_from_slice(&type_id::BOOL.to_le_bytes());
        out.push(u8::from(value));
    }

    fn push_kv_string_array(out: &mut Vec<u8>, key: &str, values: &[&str]) {
        push_string(out, key);
        out.extend_from_slice(&type_id::ARRAY.to_le_bytes());
        out.extend_from_slice(&type_id::STRING.to_le_bytes());
        out.extend_from_slice(&(values.len() as u64).to_le_bytes());
        for value in values {
            push_string(out, value);
        }
    }

    /// GGUF v3 stores tensor dimensions as uint32.
    fn push_tensor(out: &mut Vec<u8>, name: &str, dims: &[u32]) {
        push_string(out, name);
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for dim in dims {
            out.extend_from_slice(&dim.to_le_bytes());
        }
        out.extend_from_slice(&1u32.to_le_bytes()); // dtype
        out.extend_from_slice(&0u64.to_le_bytes()); // offset
    }

    /// qwen2-style header with 2 tensors totalling 1.5B parameters.
    fn sample_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&2u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&6u64.to_le_bytes()); // n_kv

        push_kv_string(&mut out, "general.architecture", "qwen2");
        push_kv_string(&mut out, "general.name", "Qwen2.5 0.5B Instruct");
        push_kv_u32(&mut out, "qwen2.context_length", 32768);
        push_kv_u32(&mut out, "qwen2.block_count", 24);
        push_kv_u32(&mut out, "general.file_type", 13); // Q4_K_M
        push_kv_bool(&mut out, "tokenizer.ggml.add_bos_token", false);

        // token_emb: 1500 x 1_000_000, output: 500 x 3_000_000  => 3.0e9 params
        push_tensor(&mut out, "token_embd.weight", &[1500, 1_000_000]);
        push_tensor(&mut out, "output.weight", &[500, 3_000_000]);
        out
    }

    #[test]
    fn parses_metadata_and_param_count() {
        let header = GgufHeader::parse_bytes(&sample_bytes()).expect("parses");
        assert_eq!(header.version, 3);
        assert_eq!(header.n_tensors, 2);
        assert_eq!(header.total_params, Some(3_000_000_000));

        let summary = header.summary();
        assert_eq!(summary.name.as_deref(), Some("Qwen2.5 0.5B Instruct"));
        assert_eq!(summary.architecture.as_deref(), Some("qwen2"));
        assert_eq!(summary.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(summary.context_length, Some(32768));
        assert_eq!(summary.block_count, Some(24));
        assert_eq!(summary.parameters_b, Some(3.0));
        assert_eq!(summary.gguf_version, 3);
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let mut bytes = sample_bytes();
        bytes[0] = b'X';
        let error = GgufHeader::parse_bytes(&bytes).expect_err("magic checked");
        assert!(error.to_string().contains("bad magic"));

        let mut bytes = sample_bytes();
        bytes[4..8].copy_from_slice(&9u32.to_le_bytes());
        let error = GgufHeader::parse_bytes(&bytes).expect_err("version checked");
        assert!(error.to_string().contains("unsupported GGUF version"));
    }

    #[test]
    fn truncates_long_string_arrays() {
        let many: Vec<String> = (0..600).map(|index| format!("tok{index}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes());
        push_kv_string_array(&mut out, "tokenizer.ggml.tokens", &refs);

        let header = GgufHeader::parse_bytes(&out).expect("parses");
        let Value::Object(object) = &header.metadata["tokenizer.ggml.tokens"] else {
            panic!("expected truncation marker, got {:?}", header.metadata);
        };
        assert_eq!(object["truncated"], Value::Bool(true));
        assert_eq!(object["count"], Value::from(600u64));
    }

    #[test]
    fn tolerates_missing_tensor_infos() {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&3u64.to_le_bytes()); // claims 3 tensors
        out.extend_from_slice(&1u64.to_le_bytes());
        push_kv_string(&mut out, "general.name", "Truncated export");

        let header = GgufHeader::parse_bytes(&out).expect("metadata survives");
        assert_eq!(header.total_params, None);
        assert_eq!(
            header.string("general.name").as_deref(),
            Some("Truncated export")
        );
    }

    #[test]
    fn chat_model_detection_uses_template_or_type() {
        let header = GgufHeader::parse_bytes(&sample_bytes()).expect("parses");
        assert!(!header.is_chat_model());

        let header = GgufHeader::parse_bytes(&with_chat_template()).expect("parses");
        assert!(header.is_chat_model());
        assert_eq!(
            header.string("tokenizer.chat_template").as_deref(),
            Some("{% for message in messages %}")
        );

        let header = GgufHeader::parse_bytes(&instruct_type_kv()).expect("parses");
        assert!(header.is_chat_model());
    }

    /// A GGUF whose KV section carries a chat template. KV pairs must come
    /// before the tensor infos, so this fixture is built from scratch.
    fn with_chat_template() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&2u64.to_le_bytes()); // n_kv
        push_kv_string(&mut out, "general.architecture", "smollm");
        push_kv_string(
            &mut out,
            "tokenizer.chat_template",
            "{% for message in messages %}",
        );
        push_tensor(&mut out, "layers.weight", &[8, 1_000_000]);
        out
    }

    /// `general.type = "instruct"` is the other signal that a file is chatable.
    fn instruct_type_kv() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&2u64.to_le_bytes()); // n_kv
        push_kv_string(&mut out, "general.architecture", "qwen2");
        push_kv_string(&mut out, "general.type", "Instruct");
        out
    }
}
