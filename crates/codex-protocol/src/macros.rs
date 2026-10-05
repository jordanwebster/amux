//! The three shapes every Codex message is built from, and the strictness
//! switch they share.
//!
//! Each shape tolerates what this crate does not know and keeps it, so a
//! line decodes and encodes back to the same JSON. In strict mode the same
//! fallbacks fail instead, which is how the recording checks see drift at
//! any depth of a message.

use std::cell::Cell;

thread_local! {
    static STRICT: Cell<bool> = const { Cell::new(false) };
}

/// Runs `decode` with every fallback turned into an error.
pub(crate) fn strictly<T>(decode: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            STRICT.with(|strict| strict.set(self.0));
        }
    }
    let _reset = Reset(STRICT.with(|strict| strict.replace(true)));
    decode()
}

pub(crate) fn is_strict() -> bool {
    STRICT.with(Cell::get)
}

/// A string-valued enum: each known spelling is a variant and any other is
/// `Other`, kept as written.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($(#[$vmeta:meta])* $variant:ident = $wire:literal,)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A spelling this crate does not know.
            Other(String),
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)*
                    Self::Other(other) => other,
                }
            }

            /// The variant written `text`; `Other` when none is.
            pub fn parse(text: &str) -> Self {
                match text {
                    $($wire => Self::$variant,)*
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Ok(match text.as_str() {
                    $($wire => Self::$variant,)*
                    _ if $crate::macros::is_strict() => {
                        return Err(::serde::de::Error::custom(format!(
                            concat!("unknown ", stringify!($name), " {:?}"),
                            text
                        )));
                    }
                    _ => Self::Other(text),
                })
            }
        }
    };
}

/// An object enum told apart by its `type` field: each known type is a
/// variant holding the rest of the object, and any other is `Unknown`,
/// holding the whole object.
macro_rules! tagged_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($(#[$vmeta:meta])* $tag:literal => $variant:ident($payload:ty),)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $name {
            $($(#[$vmeta])* $variant($payload),)*
            /// A type this crate does not know, or a known type whose fields
            /// did not decode, as written.
            Unknown(::serde_json::Map<String, ::serde_json::Value>),
        }

        impl $name {
            /// The `type` this value is written with.
            pub fn kind(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $tag,)*
                    Self::Unknown(object) => object.get("type").and_then(|kind| kind.as_str()).unwrap_or(""),
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use ::serde::ser::Error as _;
                let object = match self {
                    $(Self::$variant(payload) => {
                        let mut object = match ::serde_json::to_value(payload).map_err(S::Error::custom)? {
                            ::serde_json::Value::Object(object) => object,
                            _ => return Err(S::Error::custom(concat!(stringify!($payload), " is not an object"))),
                        };
                        object.insert("type".into(), ::serde_json::Value::String($tag.into()));
                        object
                    })*
                    Self::Unknown(object) => object.clone(),
                };
                object.serialize(serializer)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                use ::serde::de::Error as _;
                let object = ::serde_json::Map::<String, ::serde_json::Value>::deserialize(deserializer)?;
                let kind = object.get("type").and_then(|kind| kind.as_str()).unwrap_or("").to_owned();
                let mut fields = object.clone();
                fields.remove("type");
                let decoded = match kind.as_str() {
                    $($tag => ::serde_json::from_value(::serde_json::Value::Object(fields)).map(Self::$variant),)*
                    _ => Err(::serde_json::Error::custom(format!(
                        concat!("unknown ", stringify!($name), " type {:?}"),
                        kind
                    ))),
                };
                match decoded {
                    Ok(value) => Ok(value),
                    Err(error) if $crate::macros::is_strict() => Err(D::Error::custom(format!(
                        concat!(stringify!($name), " {:?}: {}"),
                        kind, error
                    ))),
                    Err(_) => Ok(Self::Unknown(object)),
                }
            }
        }
    };
}

/// A JSON-RPC method enum: each known method is a variant holding its
/// params. A method this crate does not know has no variant; the envelope
/// keeps that whole line instead.
macro_rules! method_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $($(#[$vmeta:meta])* $method:literal => $variant:ident($payload:ty),)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub enum $name {
            $($(#[$vmeta])* $variant($payload),)*
        }

        impl $name {
            /// The method this message is sent as.
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $method,)*
                }
            }

            /// None when the method is not one of these.
            pub(crate) fn from_parts(
                method: &str,
                params: ::serde_json::Value,
            ) -> Option<Result<Self, ::serde_json::Error>> {
                Some(match method {
                    $($method => ::serde_json::from_value(params).map(Self::$variant),)*
                    _ => return None,
                })
            }

            /// The params as written; `Null` means the line has none.
            pub(crate) fn params(&self) -> ::serde_json::Value {
                match self {
                    $(Self::$variant(params) => ::serde_json::to_value(params).expect("params serialize"),)*
                }
            }
        }
    };
}

pub(crate) use method_enum;
pub(crate) use string_enum;
pub(crate) use tagged_enum;
