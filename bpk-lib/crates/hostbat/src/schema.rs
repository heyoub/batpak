//! D7 schema identity: stable, language-neutral wire-shape descriptors.
//!
//! A Rust type path is **not** a wire identity. Renaming or deleting a type must
//! never change — or break — the bytes a module exchanges with its consumers.
//! S11 makes that structural: a schema is identified by a namespaced
//! [`SchemaId`] plus a monotonic [`SchemaVersion`], and its wire shape is pinned
//! by a [`CanonicalEncoding`] content hash. A given `(SchemaId, SchemaVersion)`
//! can never silently change bytes — to change the shape you must bump the
//! version (compat-matrix discipline, applied to schemas).
//!
//! Each [`SchemaDescriptor`] carries committed [`GoldenVector`]s: canonical
//! example byte vectors that witness wire stability. The
//! [`crate::schema_gate`] module turns these into the immutability gate.
//!
//! [`DiagnosticRustType`] is **informational only**: it records which Rust type
//! currently materializes a schema for human navigation. It is deliberately
//! excluded from every digest, so renaming or deleting it changes no identity —
//! this is what replaces the `refbat::*` rust-type-path-as-identity model that
//! broke when those types were deleted.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::HostError;
use crate::identity::{canonical_digest, Digest};

/// Domain separator for a schema's canonical-encoding content hash.
const SCHEMA_ENCODING_DOMAIN: &str = "hostbat.schema.v1";

/// Maximum bytes accepted for a [`SchemaId`].
const MAX_SCHEMA_ID_BYTES: usize = 256;

/// A namespaced, language-neutral stable schema name, e.g.
/// `"hostbat.op.echo.in"` or `"hostbat.event.audit"`.
///
/// The id is the *name* axis of schema identity. It is grammar-validated so the
/// same bytes are spelled the same way in every language that consumes the
/// composition manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SchemaId(String);

impl SchemaId {
    /// Construct a schema id, validating the namespaced grammar.
    ///
    /// # Errors
    /// [`HostError::SchemaInvalid`] if the id is empty, longer than 256 bytes,
    /// has a leading/trailing/doubled `.`, or contains a byte outside
    /// `[a-z0-9._-]`.
    pub fn new(id: impl Into<String>) -> Result<Self, HostError> {
        let id = id.into();
        let reject = |detail: &str| {
            Err(HostError::SchemaInvalid {
                schema: id.clone(),
                detail: detail.to_owned(),
            })
        };
        if id.is_empty() {
            return reject("empty schema id");
        }
        if id.len() > MAX_SCHEMA_ID_BYTES {
            return reject("schema id longer than 256 bytes");
        }
        if id.starts_with('.') || id.ends_with('.') {
            return reject("schema id has a leading or trailing '.'");
        }
        if id.contains("..") {
            return reject("schema id has a doubled '.'");
        }
        if !id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        }) {
            return reject("schema id has characters outside [a-z0-9._-]");
        }
        Ok(Self(id))
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SchemaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The monotonic version axis of schema identity. A change to a schema's wire
/// shape requires a new version; the same `(SchemaId, SchemaVersion)` always
/// names the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SchemaVersion(pub u32);

impl SchemaVersion {
    /// The raw monotonic version number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// The content hash over a schema's canonical wire shape — the **immutability
/// gate**. Two descriptors with the same `(SchemaId, SchemaVersion)` must carry
/// the same `CanonicalEncoding`, or one of them silently changed the bytes.
///
/// The hash is computed the family-canonical way: BLAKE3 over a domain-separated
/// canonical encoding of the schema's declared shape and golden vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalEncoding(Digest);

impl CanonicalEncoding {
    /// The raw 32-byte content hash.
    #[must_use]
    pub const fn bytes(&self) -> &Digest {
        &self.0
    }

    /// Lowercase-hex rendering of the content hash.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in &self.0 {
            out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        out
    }
}

impl std::fmt::Display for CanonicalEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// One committed canonical example of a schema's wire bytes — the wire-stability
/// witness. A golden vector names a case and pins the exact canonical bytes the
/// schema produces for it; the immutability gate fails if any committed vector no
/// longer reproduces its bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GoldenVector {
    /// Stable case name (unique within a schema), e.g. `"empty"` or
    /// `"max-fields"`.
    pub case: String,
    /// The exact canonical wire bytes for this case.
    pub bytes: Vec<u8>,
}

impl GoldenVector {
    /// Construct a golden vector from a case name and its canonical bytes.
    pub fn new(case: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            case: case.into(),
            bytes: bytes.into(),
        }
    }
}

/// An *informational-only* Rust type path recording which type currently
/// materializes a schema. It is **never** folded into any digest: renaming or
/// removing it changes no identity. This is the non-load-bearing replacement for
/// the `refbat::*` rust-type-path-as-identity model.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagnosticRustType(String);

impl DiagnosticRustType {
    /// Record a diagnostic Rust type path. Not validated, not identity-bearing.
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// The recorded type path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DiagnosticRustType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The role a schema plays on the wire — operation input/output, an exported
/// event payload, or a receipt payload. Generic/mechanism, no domain meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaRole {
    /// The input shape of an operation.
    OperationInput,
    /// The output shape of an operation.
    OperationOutput,
    /// The payload shape of an exported event.
    EventPayload,
    /// The payload shape of a receipt extension.
    ReceiptPayload,
    /// The payload shape of a declared subscription stream (projection/status).
    SubscriptionPayload,
}

impl SchemaRole {
    /// Stable lowercase spelling used in the canonical encoding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OperationInput => "operation-input",
            Self::OperationOutput => "operation-output",
            Self::EventPayload => "event-payload",
            Self::ReceiptPayload => "receipt-payload",
            Self::SubscriptionPayload => "subscription-payload",
        }
    }
}

impl std::fmt::Display for SchemaRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The identity-bearing, language-neutral declaration of one wire schema.
///
/// Identity is `(id, version, role)` plus the [`CanonicalEncoding`] over the
/// declared shape and golden vectors. The [`DiagnosticRustType`] rides along but
/// is excluded from the encoding, so it can be renamed or dropped without
/// changing any byte of identity.
#[derive(Clone, Debug)]
pub struct SchemaDescriptor {
    id: SchemaId,
    version: SchemaVersion,
    role: SchemaRole,
    golden: Vec<GoldenVector>,
    diagnostic_rust_type: Option<DiagnosticRustType>,
    encoding: CanonicalEncoding,
}

/// Runtime registry for resolving schema refs and validating canonical bytes.
#[derive(Clone, Debug, Default)]
pub struct SchemaRegistry {
    by_ref: BTreeMap<(String, SchemaRole), Vec<SchemaDescriptor>>,
}

/// Domain-separated canonical view of a schema's identity-bearing shape. The
/// [`DiagnosticRustType`] is intentionally absent — it is not identity.
#[derive(Serialize)]
struct SchemaEncodingView<'a> {
    domain: &'a str,
    id: &'a SchemaId,
    version: SchemaVersion,
    role: &'a str,
    golden: &'a [GoldenVector],
}

fn compute_encoding(
    id: &SchemaId,
    version: SchemaVersion,
    role: SchemaRole,
    golden: &[GoldenVector],
) -> Result<CanonicalEncoding, HostError> {
    let view = SchemaEncodingView {
        domain: SCHEMA_ENCODING_DOMAIN,
        id,
        version,
        role: role.as_str(),
        golden,
    };
    canonical_digest(&view).map(CanonicalEncoding)
}

impl SchemaDescriptor {
    /// Declare a schema. Golden vectors are sorted by case name (so declaration
    /// order does not affect identity) and rejected if two share a case name.
    /// The canonical encoding is computed once and stored.
    ///
    /// # Errors
    /// [`HostError::SchemaInvalid`] if two golden vectors share a case name;
    /// [`HostError::CanonicalEncoding`] if encoding fails.
    pub fn new(
        id: SchemaId,
        version: SchemaVersion,
        role: SchemaRole,
        golden: Vec<GoldenVector>,
    ) -> Result<Self, HostError> {
        let mut golden = golden;
        golden.sort_by(|a, b| a.case.cmp(&b.case));
        for pair in golden.windows(2) {
            if let [a, b] = pair {
                if a.case == b.case {
                    return Err(HostError::SchemaInvalid {
                        schema: id.as_str().to_owned(),
                        detail: format!("golden vector case {:?} is declared twice", a.case),
                    });
                }
            }
        }
        let encoding = compute_encoding(&id, version, role, &golden)?;
        Ok(Self {
            id,
            version,
            role,
            golden,
            diagnostic_rust_type: None,
            encoding,
        })
    }

    /// Attach an informational-only Rust type path. Does **not** change identity.
    #[must_use]
    pub fn with_diagnostic_rust_type(mut self, rust_type: DiagnosticRustType) -> Self {
        self.diagnostic_rust_type = Some(rust_type);
        self
    }

    /// The namespaced stable schema id.
    #[must_use]
    pub fn id(&self) -> &SchemaId {
        &self.id
    }

    /// The monotonic schema version.
    #[must_use]
    pub fn version(&self) -> SchemaVersion {
        self.version
    }

    /// The wire role this schema plays.
    #[must_use]
    pub fn role(&self) -> SchemaRole {
        self.role
    }

    /// The content hash pinning the schema's wire shape — the immutability gate.
    #[must_use]
    pub fn encoding(&self) -> CanonicalEncoding {
        self.encoding
    }

    /// The committed golden vectors, in canonical (case-name) order.
    pub fn golden(&self) -> impl Iterator<Item = &GoldenVector> {
        self.golden.iter()
    }

    /// The informational-only Rust type path, if recorded.
    #[must_use]
    pub fn diagnostic_rust_type(&self) -> Option<&DiagnosticRustType> {
        self.diagnostic_rust_type.as_ref()
    }

    /// The canonical `(id, version, role)` identity key.
    #[must_use]
    pub(crate) fn identity_key(&self) -> (&str, u32, SchemaRole) {
        (self.id.as_str(), self.version.0, self.role)
    }

    /// Recompute the canonical encoding from the stored shape and compare it to
    /// the sealed encoding. `false` means the declared shape no longer hashes to
    /// the committed encoding (a silent wire-shape change at a fixed version).
    ///
    /// # Errors
    /// [`HostError::CanonicalEncoding`] if re-encoding fails.
    pub fn verify_encoding(&self) -> Result<bool, HostError> {
        let recomputed = compute_encoding(&self.id, self.version, self.role, &self.golden)?;
        Ok(recomputed == self.encoding)
    }

    /// Serializable, identity-bearing manifest view (no diagnostic type). Used to
    /// fold schemas into the module-manifest digest.
    pub(crate) fn manifest_view(&self) -> SchemaManifestView<'_> {
        SchemaManifestView {
            id: &self.id,
            version: self.version,
            role: self.role.as_str(),
            encoding: *self.encoding.bytes(),
        }
    }

    /// Corrupt the sealed encoding to a value the declared shape no longer
    /// reproduces. **Test-only**, behind the gauntlet red-fixture cfg: feeds the
    /// schema-immutability fixture that proves a silent byte change is caught.
    #[cfg(any(test, gauntlet_red_fixture))]
    pub(crate) fn corrupt_encoding_for_fixture(&mut self) {
        let mut bytes = self.encoding.0;
        bytes[0] ^= 0xff;
        self.encoding = CanonicalEncoding(bytes);
    }
}

impl SchemaRegistry {
    /// Build a registry from composition-resolved schema descriptors.
    pub fn from_descriptors<I>(descriptors: I) -> Self
    where
        I: IntoIterator<Item = SchemaDescriptor>,
    {
        let mut by_ref = BTreeMap::<(String, SchemaRole), Vec<SchemaDescriptor>>::new();
        for descriptor in descriptors {
            by_ref
                .entry((descriptor.id().as_str().to_owned(), descriptor.role()))
                .or_default()
                .push(descriptor);
        }
        for descriptors in by_ref.values_mut() {
            descriptors.sort_by_key(|descriptor| descriptor.version().get());
        }
        Self { by_ref }
    }

    /// Validate bytes against the v1 runtime schema contract.
    ///
    /// This checks descriptor presence, unique schema ref resolution for the
    /// requested role, descriptor encoding integrity, committed golden-vector
    /// canonical decode, and payload canonical decode. It does not claim full
    /// structural field/type validation until a descriptor carries a structural
    /// validator.
    ///
    /// IMPORTANT: the payload check proves the bytes are well-formed canonical
    /// MessagePack — it is schema-INDEPENDENT. Any canonical payload passes
    /// regardless of `schema_id`; this verifies descriptor integrity + payload
    /// well-formedness, NOT that the payload conforms to the schema's shape.
    ///
    /// # Errors
    /// [`HostError::SchemaValidation`] if the schema cannot be resolved or the
    /// bytes fail the v1 validation contract; [`HostError::CanonicalEncoding`]
    /// if descriptor re-encoding fails.
    pub fn validate(
        &self,
        schema_id: &str,
        role: SchemaRole,
        bytes: &[u8],
    ) -> Result<(), HostError> {
        let descriptor = self.resolve(schema_id, role)?;
        if !descriptor.verify_encoding()? {
            return Err(schema_validation(
                schema_id,
                role,
                "descriptor encoding no longer matches its declared shape",
            ));
        }
        for golden in descriptor.golden() {
            decode_canonical(&golden.bytes).map_err(|detail| {
                schema_validation(
                    schema_id,
                    role,
                    format!(
                        "golden vector {:?} is not canonical bytes: {detail}",
                        golden.case
                    ),
                )
            })?;
        }
        decode_canonical(bytes).map_err(|detail| {
            schema_validation(
                schema_id,
                role,
                format!("payload is not canonical bytes: {detail}"),
            )
        })
    }

    fn resolve(&self, schema_id: &str, role: SchemaRole) -> Result<&SchemaDescriptor, HostError> {
        let Some(descriptors) = self.by_ref.get(&(schema_id.to_owned(), role)) else {
            return Err(schema_validation(
                schema_id,
                role,
                "required descriptor is not present",
            ));
        };
        if descriptors.len() != 1 {
            let versions: Vec<String> = descriptors
                .iter()
                .map(|descriptor| descriptor.version().get().to_string())
                .collect();
            return Err(schema_validation(
                schema_id,
                role,
                format!(
                    "schema ref is ambiguous across versions [{}]",
                    versions.join(", ")
                ),
            ));
        }
        descriptors
            .first()
            .ok_or_else(|| schema_validation(schema_id, role, "required descriptor is not present"))
    }
}

fn decode_canonical(bytes: &[u8]) -> Result<(), String> {
    batpak::canonical::from_bytes::<serde::de::IgnoredAny>(bytes)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn schema_validation(
    schema: impl Into<String>,
    role: SchemaRole,
    detail: impl Into<String>,
) -> HostError {
    HostError::SchemaValidation {
        schema: schema.into(),
        role: role.to_string(),
        detail: detail.into(),
    }
}

/// The identity-bearing manifest view of a schema: id, version, role, and the
/// content hash. The diagnostic Rust type is deliberately excluded so it never
/// touches a module or composition digest.
#[derive(Serialize)]
pub(crate) struct SchemaManifestView<'a> {
    id: &'a SchemaId,
    version: SchemaVersion,
    role: &'a str,
    encoding: Digest,
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod schema_tests;
