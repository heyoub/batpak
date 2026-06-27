//! Language-neutral structural schema shapes and runtime validation.
//!
//! Shapes are identity-bearing via the parent [`SchemaDescriptor`] digest
//! (`hostbat.schema.v2`). [`DiagnosticRustType`] is never part of a shape view.

use std::collections::BTreeSet;

use rmpv::Value;
use serde::Serialize;

use crate::error::HostError;
use crate::schema::{SchemaRegistry, SchemaRole};

const MAX_REF_DEPTH: usize = 32;

/// Structural wire shape for one schema declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SchemaShape {
    /// A scalar wire value with optional bounds and nullability.
    Scalar(ScalarShape),
    /// A named-field MessagePack map with a fixed field set.
    Record(RecordShape),
    /// A MessagePack array with homogeneous elements.
    List(ListShape),
    /// A MessagePack map with typed keys and values.
    Map(MapShape),
    /// A MessagePack array with fixed positional elements.
    Tuple(TupleShape),
    /// A string-valued scalar restricted to declared variants.
    StringEnum(StringEnumShape),
    /// Indirection to another schema identity resolved through [`SchemaRegistry`].
    Ref(RefShape),
}

impl SchemaShape {
    /// Unbounded UTF-8 string scalar.
    #[must_use]
    pub fn string() -> Self {
        Self::Scalar(ScalarShape::string())
    }

    /// Validate one decoded MessagePack value against this shape.
    ///
    /// # Errors
    /// Returns a human-readable detail string on structural mismatch.
    pub fn validate(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
    ) -> Result<(), String> {
        self.validate_depth(registry, role, value, 0)
    }

    fn validate_depth(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_REF_DEPTH {
            return Err("schema ref nesting exceeds maximum depth".to_owned());
        }
        match self {
            Self::Scalar(shape) => shape.validate(value),
            Self::Record(shape) => shape.validate(registry, role, value, depth),
            Self::List(shape) => shape.validate(registry, role, value, depth),
            Self::Map(shape) => shape.validate(registry, role, value, depth),
            Self::Tuple(shape) => shape.validate(registry, role, value, depth),
            Self::StringEnum(shape) => shape.validate(value),
            Self::Ref(shape) => shape.validate(registry, value, depth),
        }
    }
}

/// Scalar kind for [`SchemaShape::Scalar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScalarKind {
    /// Boolean scalar.
    Bool,
    /// Signed 64-bit integer scalar.
    I64,
    /// Unsigned 64-bit integer scalar.
    U64,
    /// IEEE-754 double scalar.
    F64,
    /// UTF-8 string scalar.
    String,
    /// Opaque bytes scalar.
    Bytes,
}

/// Bounds and nullability for a scalar shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScalarShape {
    /// Wire scalar kind.
    pub kind: ScalarKind,
    /// Whether MessagePack nil is accepted.
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Minimum byte or UTF-8 length for string/bytes scalars.
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Maximum byte or UTF-8 length for string/bytes scalars.
    pub max_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Minimum inclusive i64 value.
    pub min_i64: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Maximum inclusive i64 value.
    pub max_i64: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Minimum inclusive u64 value.
    pub min_u64: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Maximum inclusive u64 value.
    pub max_u64: Option<u64>,
}

impl ScalarShape {
    /// Unbounded string scalar, non-nullable.
    #[must_use]
    pub fn string() -> Self {
        Self {
            kind: ScalarKind::String,
            nullable: false,
            min_length: None,
            max_length: None,
            min_i64: None,
            max_i64: None,
            min_u64: None,
            max_u64: None,
        }
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        if value.is_nil() {
            if self.nullable {
                return Ok(());
            }
            return Err("nil is not allowed for non-nullable scalar".to_owned());
        }
        match self.kind {
            ScalarKind::Bool => validate_bool_scalar(value),
            ScalarKind::I64 => self.validate_i64_scalar(value),
            ScalarKind::U64 => self.validate_u64_scalar(value),
            ScalarKind::F64 => validate_f64_scalar(value),
            ScalarKind::String => self.validate_string_scalar(value),
            ScalarKind::Bytes => self.validate_bytes_scalar(value),
        }
    }

    fn validate_i64_scalar(&self, value: &Value) -> Result<(), String> {
        let Value::Integer(integer) = value else {
            return Err("expected i64 scalar".to_owned());
        };
        let number = integer
            .as_i64()
            .ok_or_else(|| "integer does not fit i64".to_owned())?;
        if let Some(min) = self.min_i64 {
            if number < min {
                return Err(format!("i64 below minimum {min}"));
            }
        }
        if let Some(max) = self.max_i64 {
            if number > max {
                return Err(format!("i64 above maximum {max}"));
            }
        }
        Ok(())
    }

    fn validate_u64_scalar(&self, value: &Value) -> Result<(), String> {
        let Value::Integer(integer) = value else {
            return Err("expected u64 scalar".to_owned());
        };
        let number = integer
            .as_u64()
            .ok_or_else(|| "integer is not non-negative u64".to_owned())?;
        if let Some(min) = self.min_u64 {
            if number < min {
                return Err(format!("u64 below minimum {min}"));
            }
        }
        if let Some(max) = self.max_u64 {
            if number > max {
                return Err(format!("u64 above maximum {max}"));
            }
        }
        Ok(())
    }

    fn validate_string_scalar(&self, value: &Value) -> Result<(), String> {
        let Value::String(text) = value else {
            return Err("expected string scalar".to_owned());
        };
        let text = text
            .as_str()
            .ok_or_else(|| "string must be utf-8".to_owned())?;
        let len = u32::try_from(text.len()).map_err(|_| "string length exceeds u32".to_owned())?;
        check_bounds(len, self.min_length, self.max_length, "string")
    }

    fn validate_bytes_scalar(&self, value: &Value) -> Result<(), String> {
        let Value::Binary(bytes) = value else {
            return Err("expected bytes scalar".to_owned());
        };
        let len = u32::try_from(bytes.len()).map_err(|_| "bytes length exceeds u32".to_owned())?;
        check_bounds(len, self.min_length, self.max_length, "bytes")
    }
}

fn validate_bool_scalar(value: &Value) -> Result<(), String> {
    match value {
        Value::Boolean(_) => Ok(()),
        Value::Nil
        | Value::Integer(_)
        | Value::F32(_)
        | Value::F64(_)
        | Value::String(_)
        | Value::Binary(_)
        | Value::Array(_)
        | Value::Map(_)
        | Value::Ext(_, _) => Err("expected bool scalar".to_owned()),
    }
}

fn validate_f64_scalar(value: &Value) -> Result<(), String> {
    match value {
        Value::F64(_) => Ok(()),
        Value::Nil
        | Value::Boolean(_)
        | Value::Integer(_)
        | Value::F32(_)
        | Value::String(_)
        | Value::Binary(_)
        | Value::Array(_)
        | Value::Map(_)
        | Value::Ext(_, _) => Err("expected f64 scalar".to_owned()),
    }
}

/// One field in a [`RecordShape`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordField {
    /// Stable field name in the named-field map.
    pub name: String,
    /// Structural shape of the field value.
    pub shape: SchemaShape,
    /// When true, absence is allowed; nil still requires nullable inner shapes.
    pub optional: bool,
}

impl RecordField {
    /// Required record field.
    #[must_use]
    pub fn required(name: impl Into<String>, shape: SchemaShape) -> Self {
        Self {
            name: name.into(),
            shape,
            optional: false,
        }
    }

    /// Optional record field (absent allowed; nil needs nullable on the inner shape).
    #[must_use]
    pub fn optional(name: impl Into<String>, shape: SchemaShape) -> Self {
        Self {
            name: name.into(),
            shape,
            optional: true,
        }
    }
}

/// Named-field map shape. Field names are canonicalized for identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordShape {
    /// Canonically ordered declared fields.
    pub fields: Vec<RecordField>,
}

impl RecordShape {
    /// Construct a record shape with canonical field ordering and duplicate rejection.
    ///
    /// # Errors
    /// [`HostError::SchemaInvalid`] when two fields share a name.
    pub fn new(schema: &str, mut fields: Vec<RecordField>) -> Result<Self, HostError> {
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        for pair in fields.windows(2) {
            if let [a, b] = pair {
                if a.name == b.name {
                    return Err(HostError::SchemaInvalid {
                        schema: schema.to_owned(),
                        detail: format!("duplicate record field {:?}", a.name),
                    });
                }
            }
        }
        Ok(Self { fields })
    }

    fn validate(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        let Value::Map(entries) = value else {
            return Err("expected record map".to_owned());
        };
        let mut seen = BTreeSet::new();
        for (key, field_value) in entries {
            let Value::String(name) = key else {
                return Err("record field keys must be strings".to_owned());
            };
            let name = name
                .as_str()
                .ok_or_else(|| "record field name must be utf-8".to_owned())?;
            if !seen.insert(name.to_owned()) {
                return Err(format!("duplicate record field {name:?}"));
            }
            let Some(field) = self.fields.iter().find(|field| field.name == name) else {
                return Err(format!("unknown record field {name:?}"));
            };
            field
                .shape
                .validate_depth(registry, role, field_value, depth + 1)?;
        }
        for field in &self.fields {
            if field.optional {
                continue;
            }
            if !seen.contains(&field.name) {
                return Err(format!("missing required record field {:?}", field.name));
            }
        }
        Ok(())
    }
}

/// Homogeneous list shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListShape {
    /// Shape shared by every list element.
    pub element: Box<SchemaShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Minimum inclusive element count.
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Maximum inclusive element count.
    pub max_length: Option<u32>,
}

impl ListShape {
    /// Construct a list shape.
    #[must_use]
    pub fn new(element: SchemaShape) -> Self {
        Self {
            element: Box::new(element),
            min_length: None,
            max_length: None,
        }
    }

    fn validate(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        let Value::Array(items) = value else {
            return Err("expected list array".to_owned());
        };
        let len = u32::try_from(items.len()).map_err(|_| "list length exceeds u32".to_owned())?;
        check_bounds(len, self.min_length, self.max_length, "list")?;
        for item in items {
            self.element
                .validate_depth(registry, role, item, depth + 1)?;
        }
        Ok(())
    }
}

/// Homogeneous map shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MapShape {
    /// Shape every map key must satisfy.
    pub key: Box<SchemaShape>,
    /// Shape every map value must satisfy.
    pub value: Box<SchemaShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Minimum inclusive entry count.
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Maximum inclusive entry count.
    pub max_length: Option<u32>,
}

impl MapShape {
    /// Construct a map shape.
    #[must_use]
    pub fn new(key: SchemaShape, value: SchemaShape) -> Self {
        Self {
            key: Box::new(key),
            value: Box::new(value),
            min_length: None,
            max_length: None,
        }
    }

    fn validate(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        let Value::Map(entries) = value else {
            return Err("expected map".to_owned());
        };
        let len = u32::try_from(entries.len()).map_err(|_| "map length exceeds u32".to_owned())?;
        check_bounds(len, self.min_length, self.max_length, "map")?;
        for (key, item) in entries {
            self.key.validate_depth(registry, role, key, depth + 1)?;
            self.value.validate_depth(registry, role, item, depth + 1)?;
        }
        Ok(())
    }
}

/// Fixed-length tuple shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TupleShape {
    /// Positional element shapes in declaration order.
    pub elements: Vec<SchemaShape>,
}

impl TupleShape {
    /// Construct a tuple shape.
    #[must_use]
    pub fn new(elements: Vec<SchemaShape>) -> Self {
        Self { elements }
    }

    fn validate(
        &self,
        registry: &SchemaRegistry,
        role: SchemaRole,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        let Value::Array(items) = value else {
            return Err("expected tuple array".to_owned());
        };
        if items.len() != self.elements.len() {
            return Err(format!(
                "tuple length {} does not match expected {}",
                items.len(),
                self.elements.len()
            ));
        }
        for (element, item) in self.elements.iter().zip(items.iter()) {
            element.validate_depth(registry, role, item, depth + 1)?;
        }
        Ok(())
    }
}

/// String-valued enumeration shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StringEnumShape {
    /// Canonically ordered allowed string variants.
    pub variants: Vec<String>,
}

impl StringEnumShape {
    /// Construct a string enum with canonical variant ordering and duplicate rejection.
    ///
    /// # Errors
    /// [`HostError::SchemaInvalid`] when two variants share the same spelling.
    pub fn new(schema: &str, mut variants: Vec<String>) -> Result<Self, HostError> {
        variants.sort();
        for pair in variants.windows(2) {
            if let [a, b] = pair {
                if a == b {
                    return Err(HostError::SchemaInvalid {
                        schema: schema.to_owned(),
                        detail: format!("duplicate string enum variant {a:?}"),
                    });
                }
            }
        }
        Ok(Self { variants })
    }

    fn validate(&self, value: &Value) -> Result<(), String> {
        let Value::String(text) = value else {
            return Err("expected string enum value".to_owned());
        };
        let text = text
            .as_str()
            .ok_or_else(|| "string enum value must be utf-8".to_owned())?;
        if self.variants.iter().any(|variant| variant == text) {
            Ok(())
        } else {
            Err(format!("string enum value {text:?} is not declared"))
        }
    }
}

/// Reference to another schema identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefShape {
    /// Target schema id resolved through [`SchemaRegistry`].
    pub schema_id: String,
    /// Target schema role paired with [`Self::schema_id`].
    pub role: SchemaRole,
}

impl RefShape {
    /// Construct a ref shape.
    #[must_use]
    pub fn new(schema_id: impl Into<String>, role: SchemaRole) -> Self {
        Self {
            schema_id: schema_id.into(),
            role,
        }
    }

    fn validate(
        &self,
        registry: &SchemaRegistry,
        value: &Value,
        depth: usize,
    ) -> Result<(), String> {
        let descriptor = registry
            .resolve_descriptor(&self.schema_id, self.role)
            .map_err(|error| error.to_string())?;
        let Some(shape) = descriptor.shape() else {
            return Err(format!(
                "referenced schema {} has no structural shape",
                self.schema_id
            ));
        };
        shape.validate_depth(registry, self.role, value, depth + 1)
    }
}

fn check_bounds(len: u32, min: Option<u32>, max: Option<u32>, label: &str) -> Result<(), String> {
    if let Some(min) = min {
        if len < min {
            return Err(format!("{label} length below minimum {min}"));
        }
    }
    if let Some(max) = max {
        if len > max {
            return Err(format!("{label} length above maximum {max}"));
        }
    }
    Ok(())
}

/// Decode canonical MessagePack bytes to an [`rmpv::Value`] for structural validation.
pub(crate) fn decode_structural_value(bytes: &[u8]) -> Result<Value, String> {
    use std::io::Cursor;

    rmpv::decode::read_value(&mut Cursor::new(bytes)).map_err(|error| error.to_string())
}
