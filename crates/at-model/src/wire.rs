//! Shared machinery for enums with an explicit wire-name table.

/// Returned when a string is not one of an enum's wire names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {type_name} `{input}`; expected one of: {}", .expected.join(", "))]
pub struct ParseWireError {
    /// Name of the enum being parsed.
    pub type_name: &'static str,
    /// The rejected input.
    pub input: String,
    /// Every accepted wire name.
    pub expected: &'static [&'static str],
}

/// Declares a unit enum whose serde names, `Display` and `FromStr` all come from one table.
///
/// `FromStr` ignores ASCII case; serde accepts only the exact wire name.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        $vis enum $name {
            $( $(#[$vmeta])* #[serde(rename = $wire)] $variant ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant ),+ ];
            /// Every wire name, in declaration order.
            pub const WIRE_NAMES: &'static [&'static str] = &[ $( $wire ),+ ];

            /// The stable wire name used in JSON, SQLite and the CLI.
            #[must_use]
            pub const fn wire_name(self) -> &'static str {
                match self { $( $name::$variant => $wire ),+ }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.wire_name())
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::wire::ParseWireError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $( if s.eq_ignore_ascii_case($wire) { return Ok($name::$variant); } )+
                Err($crate::wire::ParseWireError {
                    type_name: stringify!($name),
                    input: s.to_owned(),
                    expected: Self::WIRE_NAMES,
                })
            }
        }
    };
}

pub(crate) use wire_enum;
