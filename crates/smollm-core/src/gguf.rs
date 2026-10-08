//! Minimal, dependency-free GGUF metadata reader.
//!
//! Supports the GGUF v2/v3 header: key/value metadata plus tensor-info
//! parsing for a parameter count and for where the tensor data begins, which
//! turns a file size into an exact weight size. Tensor *data* is never read, so
//! probing a multi-gigabyte file stays cheap.

use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
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
/// `general.alignment` default from the GGUF spec: tensor data starts on a
/// 32-byte boundary after the header.
const DEFAULT_ALIGNMENT: u64 = 32;
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

/// Quantisation names indexed by `general.file_type`, which is llama.cpp's
/// `llama_ftype` *enum*, not a dense list.
///
/// Position matters: the enum keeps holes where formats were removed (4-6,
/// 33-35), so a compact table silently shifts every name after them and labels
/// a Q4_K_M file as Q5_K_M. `None` marks those gaps — an index with no name is
/// reported as unknown rather than guessed at. Values match llama.cpp's
/// `include/llama.h`; newer types beyond the end simply read as unknown.
const FILE_TYPES: [Option<&str>; 42] = [
    Some("F32"),       // 0
    Some("F16"),       // 1
    Some("Q4_0"),      // 2
    Some("Q4_1"),      // 3
    None,              // 4  removed
    None,              // 5  removed
    None,              // 6  removed
    Some("Q8_0"),      // 7
    Some("Q5_0"),      // 8
    Some("Q5_1"),      // 9
    Some("Q2_K"),      // 10
    Some("Q3_K_S"),    // 11
    Some("Q3_K_M"),    // 12
    Some("Q3_K_L"),    // 13
    Some("Q4_K_S"),    // 14
    Some("Q4_K_M"),    // 15
    Some("Q5_K_S"),    // 16
    Some("Q5_K_M"),    // 17
    Some("Q6_K"),      // 18
    Some("IQ2_XXS"),   // 19
    Some("IQ2_XS"),    // 20
    Some("Q2_K_S"),    // 21
    Some("IQ3_XS"),    // 22
    Some("IQ3_XXS"),   // 23
    Some("IQ1_S"),     // 24
    Some("IQ4_NL"),    // 25
    Some("IQ3_S"),     // 26
    Some("IQ3_M"),     // 27
    Some("IQ2_S"),     // 28
    Some("IQ2_M"),     // 29
    Some("IQ4_XS"),    // 30
    Some("IQ1_M"),     // 31
    Some("BF16"),      // 32
    None,              // 33 removed
    None,              // 34 removed
    None,              // 35 removed
    Some("TQ1_0"),     // 36
    Some("TQ2_0"),     // 37
    Some("MXFP4_MOE"), // 38
    Some("NVFP4"),     // 39
    Some("Q1_0"),      // 40
    Some("Q2_0"),      // 41
];

/// A parsed GGUF header.
#[derive(Debug, Clone)]
pub struct GgufHeader {
    pub version: u32,
    pub n_tensors: u64,
    pub metadata: Map<String, Value>,
    /// Sum of tensor element counts, when tensor infos were readable.
    pub total_params: Option<u64>,
    /// First byte of the tensor data section: the header length rounded up to
    /// `general.alignment`. `None` when the tensor infos could not be read.
    pub data_start: Option<u64>,
    /// Size in bytes of the file or buffer the header came from, 0 when the
    /// caller read through a bare reader that reports no length.
    pub source_bytes: u64,
}

impl GgufHeader {
    /// Read header + metadata + tensor infos from a file on disk.
    pub fn read(path: &Path) -> AppResult<Self> {
        let file = std::fs::File::open(path).map_err(|source| {
            AppError::GgufParse(format!("cannot open {}: {source}", path.display()))
        })?;
        let source_bytes = file
            .metadata()
            .map_err(|source| {
                AppError::GgufParse(format!("cannot size {}: {source}", path.display()))
            })?
            .len();
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut header = Self::parse(&mut reader)?;
        header.source_bytes = source_bytes;
        Ok(header)
    }

    /// Parse from an in-memory buffer (tests, fixtures, streamed prefixes).
    pub fn parse_bytes(bytes: &[u8]) -> AppResult<Self> {
        let mut header = Self::parse(&mut Cursor::new(bytes))?;
        header.source_bytes = bytes.len() as u64;
        Ok(header)
    }

    pub fn parse<R: Read + Seek>(reader: &mut R) -> AppResult<Self> {
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

        let alignment = metadata
            .get("general.alignment")
            .and_then(Value::as_u64)
            .filter(|bytes| *bytes > 0)
            .unwrap_or(DEFAULT_ALIGNMENT);
        let tensors_at = reader.stream_position()?;

        // Tensor infos are best effort: some quantised exports pad before them,
        // and metadata is still worth showing even if we cannot count params.
        let (total_params, data_start) = if n_tensors == 0 || n_tensors > MAX_TENSORS {
            (None, None)
        } else {
            match read_tensor_infos(reader, tensors_at, n_tensors, version, alignment) {
                Some((params, start)) => (Some(params), Some(start)),
                None => (None, None),
            }
        };

        Ok(Self {
            version,
            n_tensors,
            metadata,
            total_params,
            data_start,
            source_bytes: 0,
        })
    }

    /// Bytes the file spends on tensor weights.
    ///
    /// Everything after the aligned data start is tensor data, so this is exact
    /// for a single-file GGUF and needs no table of quantisation block sizes.
    /// `None` when either end of that range is unknown.
    #[must_use]
    pub fn weight_bytes(&self) -> Option<u64> {
        self.data_start
            .and_then(|start| self.source_bytes.checked_sub(start))
            .filter(|bytes| *bytes > 0)
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
            .flatten()
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
            head_count: self.u32(&format!("{prefix}.attention.head_count")),
            head_count_kv: self.u32(&format!("{prefix}.attention.head_count_kv")),
            weight_bytes: self.weight_bytes(),
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

fn align_up(value: u64, alignment: u64) -> u64 {
    let padding = alignment - 1;
    value.saturating_add(padding) & !padding
}

/// Count the parameters and locate the tensor data section.
///
/// The GGUF spec writes v3 tensor dimensions as uint32 and v2 ones as uint64,
/// but real v3 exports have been found using uint64 anyway, so the width the
/// version implies is tried first and the other only if that diverges: a header
/// whose tensor infos cannot be read has neither a parameter count nor a way to
/// measure its weights.
fn read_tensor_infos<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    n_tensors: u64,
    version: u32,
    alignment: u64,
) -> Option<(u64, u64)> {
    for wide_dims in [version < 3, version >= 3] {
        if reader.seek(SeekFrom::Start(start)).is_err() {
            return None;
        }
        if let Ok(total) = read_tensor_params(reader, n_tensors, wide_dims) {
            let end = reader.stream_position().ok()?;
            return Some((total, align_up(end, alignment)));
        }
    }
    None
}

/// Read one tensor info entry per tensor and sum their element counts.
fn read_tensor_params<R: Read>(reader: &mut R, n_tensors: u64, wide_dims: bool) -> AppResult<u64> {
    let mut total: u64 = 0;
    for _ in 0..n_tensors {
        let _name = read_string(reader)?;
        let n_dims = read_u32(reader)?;
        if n_dims == 0 || n_dims > MAX_TENSOR_RANK {
            return Err(AppError::GgufParse(format!(
                "implausible tensor rank {n_dims}"
            )));
        }
        let mut elements: u64 = 1;
        for _ in 0..n_dims {
            let dim = if wide_dims {
                read_u64(reader)?
            } else {
                u64::from(read_u32(reader)?)
            };
            // A zero-length tensor means the read has diverged from the file.
            if dim == 0 {
                return Err(AppError::GgufParse("zero-sized tensor dimension".into()));
            }
            elements = elements.saturating_mul(dim);
        }
        let _dtype = read_u32(reader)?;
        let _offset = read_u64(reader)?;
        total = total.saturating_add(elements);
    }
    Ok(total)
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

    /// qwen2-style header with 2 tensors totalling 3B parameters.
    fn sample_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&2u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&9u64.to_le_bytes()); // n_kv

        push_kv_string(&mut out, "general.architecture", "qwen2");
        push_kv_string(&mut out, "general.name", "Qwen2.5 0.5B Instruct");
        push_kv_u32(&mut out, "qwen2.context_length", 32768);
        push_kv_u32(&mut out, "qwen2.block_count", 24);
        push_kv_u32(&mut out, "qwen2.embedding_length", 896);
        push_kv_u32(&mut out, "qwen2.attention.head_count", 14);
        push_kv_u32(&mut out, "qwen2.attention.head_count_kv", 2);
        push_kv_u32(&mut out, "general.file_type", 15); // LLAMA_FTYPE_MOSTLY_Q4_K_M
        push_kv_bool(&mut out, "tokenizer.ggml.add_bos_token", false);

        // token_emb: 1500 x 1_000_000, output: 500 x 3_000_000  => 3.0e9 params
        push_tensor(&mut out, "token_embd.weight", &[1500, 1_000_000]);
        push_tensor(&mut out, "output.weight", &[500, 3_000_000]);
        out
    }

    #[test]
    fn the_data_section_start_is_measured_not_guessed() {
        let mut bytes = sample_bytes();
        let header = GgufHeader::parse_bytes(&bytes).expect("parses");
        let start = header.data_start.expect("two tensor infos were written");
        assert_eq!(start % 32, 0, "llama.cpp aligns the tensor data");
        assert!(
            start >= bytes.len() as u64,
            "the fixture stops at the header"
        );
        assert_eq!(
            header.weight_bytes(),
            None,
            "a header on its own holds no weights"
        );

        bytes.resize(usize::try_from(start).unwrap() + 4096, 0);
        let loaded = GgufHeader::parse_bytes(&bytes).expect("re-parses");
        assert_eq!(
            loaded.weight_bytes(),
            Some(4096),
            "weights are exactly what follows the data offset"
        );
    }

    #[test]
    fn summary_carries_the_attention_geometry() {
        let summary = GgufHeader::parse_bytes(&sample_bytes())
            .expect("parses")
            .summary();
        assert_eq!(summary.embedding_length, Some(896));
        assert_eq!(summary.head_count, Some(14));
        assert_eq!(summary.head_count_kv, Some(2));
        assert_eq!(summary.weight_bytes, None, "the fixture has no data");
    }

    /// Some v3 exports store tensor dimensions as uint64 even though the spec
    /// reserves that width for v2. Reading one narrowly diverges on a zero-sized
    /// dimension, and the reader has to notice and retry.
    #[test]
    fn reads_v3_headers_that_wrote_wide_tensor_dimensions() {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&1u64.to_le_bytes()); // n_tensors
        out.extend_from_slice(&1u64.to_le_bytes()); // n_kv
        push_kv_string(&mut out, "general.architecture", "llama");

        push_string(&mut out, "token_embd.weight");
        out.extend_from_slice(&2u32.to_le_bytes()); // n_dims
        for dim in [960u64, 49_152] {
            out.extend_from_slice(&dim.to_le_bytes());
        }
        out.extend_from_slice(&8u32.to_le_bytes()); // dtype Q8_0
        out.extend_from_slice(&0u64.to_le_bytes()); // offset

        let header = GgufHeader::parse_bytes(&out).expect("retries with wide dimensions");
        assert_eq!(header.total_params, Some(960 * 49_152));
        assert!(
            header.data_start.is_some(),
            "weights cannot be measured without the data offset"
        );
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
    fn file_type_names_follow_the_enum_holes() {
        // 4-6 and 33-35 are formats llama.cpp removed. Reading one has to yield
        // no name at all, not the name of whatever shifted into that slot.
        assert_eq!(FILE_TYPES[3], Some("Q4_1"));
        assert_eq!(FILE_TYPES[4], None);
        assert_eq!(FILE_TYPES[6], None);
        assert_eq!(FILE_TYPES[7], Some("Q8_0"));
        assert_eq!(FILE_TYPES[15], Some("Q4_K_M"));
        assert_eq!(FILE_TYPES[17], Some("Q5_K_M"));
        assert_eq!(FILE_TYPES[32], Some("BF16"));
        assert_eq!(FILE_TYPES[33], None);
        // LLAMA_FTYPE_GUESSED is 1024, and it is not a quantisation.
        assert!(FILE_TYPES.get(1024).is_none());
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
