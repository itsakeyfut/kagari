//! Cheap-to-clone shared string for UI labels.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// An immutable string that clones cheaply. `&'static str` literals (most UI
/// labels) are stored inline with zero allocation; any other string shares an
/// `Arc<str>` so clones only bump a refcount.
///
/// Equality and hashing are by string content, so the two variants interoperate
/// (a `Static` and a `Shared` holding the same text are equal and hash alike).
/// There is no `From<&str>`: it would overlap `From<&'static str>` and lose the
/// zero-allocation fast path. Build a non-static one via `String` (`s.to_string().into()`).
#[derive(Clone)]
pub enum SharedString {
    Static(&'static str),
    Shared(Arc<str>),
}

impl SharedString {
    pub fn as_str(&self) -> &str {
        match self {
            SharedString::Static(s) => s,
            SharedString::Shared(s) => s,
        }
    }
}

impl Deref for SharedString {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<&'static str> for SharedString {
    fn from(s: &'static str) -> Self {
        SharedString::Static(s)
    }
}

impl From<String> for SharedString {
    fn from(s: String) -> Self {
        SharedString::Shared(Arc::from(s))
    }
}

impl PartialEq for SharedString {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SharedString {}

impl Hash for SharedString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Display for SharedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for SharedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

// Serialize as a plain string (no enum tag), so RON/JSON round-trip cleanly (a persisted theme id or
// action name is just `"..."`, #68). Deserialization always yields the `Shared` variant — the
// `Static` fast path is for `&'static str` literals, which a deserialized value can't be.
#[cfg(feature = "serde")]
impl serde::Serialize for SharedString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SharedString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(SharedString::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn shared_string_should_equal_across_variants() {
        let a: SharedString = "label".into();
        let b: SharedString = "label".to_string().into();
        assert!(matches!(a, SharedString::Static(_)));
        assert!(matches!(b, SharedString::Shared(_)));
        assert_eq!(a, b);
    }

    #[test]
    fn shared_string_should_match_as_hash_key_across_variants() {
        let mut map: HashMap<SharedString, i32> = HashMap::new();
        map.insert("key".into(), 7);
        assert_eq!(map.get(&SharedString::from("key".to_string())), Some(&7));
    }

    #[test]
    fn shared_string_should_deref_to_str() {
        let s: SharedString = "hello".into();
        assert_eq!(s.len(), 5);
        assert!(s.starts_with("he"));
    }

    #[test]
    fn shared_string_should_display_inner_text() {
        let s: SharedString = "world".to_string().into();
        assert_eq!(format!("{s}"), "world");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn shared_string_should_roundtrip_as_plain_string() {
        // Serializes without an enum tag and round-trips by content (a Static and its deserialized
        // Shared counterpart compare equal), so persisted ids/names are just `"..."`.
        let original: SharedString = "theme-dark".into();
        let encoded = ron::to_string(&original).expect("serialize");
        assert_eq!(encoded, "\"theme-dark\"", "serialized as a bare string");
        let back: SharedString = ron::from_str(&encoded).expect("deserialize");
        assert_eq!(back, original, "round-trips by content");
    }
}
