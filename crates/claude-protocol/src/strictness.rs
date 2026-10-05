//! What a strict decode notices.
//!
//! Every place a decoder keeps something it does not know (an unknown frame
//! type, content block, stream event or enum spelling) reports it here.
//! Production decoding ignores the reports; a strict decode collects them
//! and refuses the line, which is how the recording checks see drift at any
//! depth.

use std::cell::RefCell;

thread_local! {
    static SEEN: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Notes something a decoder kept without knowing it.
pub(crate) fn unknown(what: impl FnOnce() -> String) {
    SEEN.with(|seen| {
        if let Some(seen) = seen.borrow_mut().as_mut() {
            seen.push(what());
        }
    });
}

/// Runs `decode` and returns what it kept without knowing.
pub(crate) fn noticing<T>(decode: impl FnOnce() -> T) -> (T, Vec<String>) {
    struct Restore(Option<Vec<String>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SEEN.with(|seen| *seen.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(SEEN.with(|seen| seen.borrow_mut().replace(Vec::new())));
    let value = decode();
    let noticed = SEEN
        .with(|seen| seen.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    (value, noticed)
}

/// An object enum told apart by one string field: each known value is a
/// variant holding the rest of the object, and any other is `Unknown`,
/// holding the whole object.
macro_rules! tagged_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident by $field:literal {
            $($(#[$vmeta:meta])* $tag:literal => $variant:ident($payload:ty),)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub enum $name {
            $($(#[$vmeta])* $variant($payload),)*
            /// A value this crate does not know, or a known one whose fields
            /// did not decode, as written.
            Unknown($crate::stream::types::RawFrame),
        }

        impl $name {
            /// The value of the field this object is told apart by.
            pub fn kind(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $tag,)*
                    Self::Unknown(raw) => raw.field($field).and_then(|kind| kind.as_str()).unwrap_or(""),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use ::serde::ser::Error as _;
                match self {
                    $(Self::$variant(payload) => {
                        let mut object = match ::serde_json::to_value(payload).map_err(S::Error::custom)? {
                            ::serde_json::Value::Object(object) => object,
                            _ => return Err(S::Error::custom(concat!(stringify!($payload), " is not an object"))),
                        };
                        object.insert($field.into(), ::serde_json::Value::String($tag.into()));
                        object.serialize(serializer)
                    })*
                    Self::Unknown(raw) => raw.serialize(serializer),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = ::serde_json::Value::deserialize(deserializer)?;
                let kind = raw.get($field).and_then(|kind| kind.as_str()).unwrap_or("").to_owned();
                let mut fields = raw.clone();
                if let Some(object) = fields.as_object_mut() {
                    object.remove($field);
                }
                let decoded: Result<Self, ::serde_json::Error> = match kind.as_str() {
                    $($tag => ::serde_json::from_value(fields).map(Self::$variant),)*
                    _ => Err(::serde::de::Error::custom("an unknown kind")),
                };
                Ok(decoded.unwrap_or_else(|error| {
                    $crate::strictness::unknown(|| format!(
                        concat!(stringify!($name), " {:?}: {}"),
                        kind, error
                    ));
                    Self::Unknown($crate::stream::types::RawFrame::new(raw))
                }))
            }
        }
    };
}

pub(crate) use tagged_enum;
