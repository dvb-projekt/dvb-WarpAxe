//! `json!` with the code for each entry out of line.
//!
//! `serde_json::json!` expands every `"key": value` of an object into
//! the key's `String`, the value's conversion and the map insert, all
//! inline. That is fine for a handful of keys; the firmware's
//! `/api/system/info` has several hundred, and its builder alone was
//! 47 KB of the image, about 140 bytes per key (docs/16). Here the same
//! grammar expands into one call per entry ([`__rt::put`]) and one per
//! converted expression ([`__rt::val`]), both never inlined, so an entry
//! costs its arguments and a call.
//!
//! The value built is the same: the entries are inserted in the same
//! order with the same conversions (`Into<String>` for a key,
//! `serde_json::to_value` for an expression), which the tests check
//! against the original macro form by form.
//!
//! The macro rules are serde_json's (`src/macros.rs` of serde_json
//! 1.0.151, copyright its authors, MIT OR Apache-2.0), changed only
//! where an entry is inserted and where an expression is converted.

#![forbid(unsafe_code)]

/// What the macro expands to. Not for use by hand.
#[doc(hidden)]
pub mod __rt {
    pub use serde_json::{Map, Value};
    pub use std::vec;

    /// One entry into the object under construction; as the original, a
    /// repeated key replaces the earlier entry.
    #[inline(never)]
    pub fn put<K: Into<String>>(object: &mut Map<String, Value>, key: K, value: Value) {
        let _ = object.insert(key.into(), value);
    }

    /// An expression as a value; as the original, a type that cannot be
    /// represented (a map with keys that are not strings) panics.
    #[inline(never)]
    pub fn val<T: serde::Serialize + ?Sized>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }
}

/// Build a `serde_json::Value` from a JSON literal, as
/// `serde_json::json!` does:
///
/// ```
/// let code = 200;
/// let v = warpaxe_jsonval::json!({
///     "code": code,
///     "ok": true,
///     "list": [1, "two", null],
///     "inner": { "half": code / 2 },
/// });
/// assert_eq!(v, serde_json::json!({
///     "code": 200, "ok": true, "list": [1, "two", null], "inner": { "half": 100 },
/// }));
/// ```
#[macro_export]
macro_rules! json {
    ($($json:tt)+) => {
        $crate::__json!($($json)+)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json {
    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an array [...]. Produces a vec![...]
    // of the elements.
    //
    // Must be invoked as: __json!(@array [] $($tt)*)
    //////////////////////////////////////////////////////////////////////////

    // Done with trailing comma.
    (@array [$($elems:expr,)*]) => {
        $crate::__rt::vec![$($elems,)*]
    };

    // Done without trailing comma.
    (@array [$($elems:expr),*]) => {
        $crate::__rt::vec![$($elems),*]
    };

    // Next element is `null`.
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!(null)] $($rest)*)
    };

    // Next element is `true`.
    (@array [$($elems:expr,)*] true $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!(true)] $($rest)*)
    };

    // Next element is `false`.
    (@array [$($elems:expr,)*] false $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!(false)] $($rest)*)
    };

    // Next element is an array.
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!([$($array)*])] $($rest)*)
    };

    // Next element is a map.
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!({$($map)*})] $($rest)*)
    };

    // Next element is an expression followed by comma.
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!($next),] $($rest)*)
    };

    // Last element is an expression with no trailing comma.
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::__json!(@array [$($elems,)* $crate::__json!($last)])
    };

    // Comma after the most recent element.
    (@array [$($elems:expr),*] , $($rest:tt)*) => {
        $crate::__json!(@array [$($elems,)*] $($rest)*)
    };

    // Unexpected token after most recent element.
    (@array [$($elems:expr),*] $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected)
    };

    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an object {...}. Each entry is
    // inserted into the given map variable.
    //
    // Must be invoked as: __json!(@object $map () ($($tt)*) ($($tt)*))
    //
    // We require two copies of the input tokens so that we can match on one
    // copy and trigger errors on the other copy.
    //////////////////////////////////////////////////////////////////////////

    // Done.
    (@object $object:ident () () ()) => {};

    // Insert the current entry followed by trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        $crate::__rt::put(&mut $object, $($key)+, $value);
        $crate::__json!(@object $object () ($($rest)*) ($($rest)*));
    };

    // Current entry followed by unexpected token.
    (@object $object:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected);
    };

    // Insert the last entry without trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        $crate::__rt::put(&mut $object, $($key)+, $value);
    };

    // Next value is `null`.
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!(null)) $($rest)*);
    };

    // Next value is `true`.
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!(true)) $($rest)*);
    };

    // Next value is `false`.
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!(false)) $($rest)*);
    };

    // Next value is an array.
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!([$($array)*])) $($rest)*);
    };

    // Next value is a map.
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!({$($map)*})) $($rest)*);
    };

    // Next value is an expression followed by comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!($value)) , $($rest)*);
    };

    // Last value is an expression with no trailing comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::__json!(@object $object [$($key)+] ($crate::__json!($value)));
    };

    // Missing value for last entry. Trigger a reasonable error message.
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::__json!();
    };

    // Missing colon and value for last entry. Trigger a reasonable error
    // message.
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::__json!();
    };

    // Misplaced colon. Trigger a reasonable error message.
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `:`".
        $crate::__json_unexpected!($colon);
    };

    // Found a comma inside a key. Trigger a reasonable error message.
    (@object $object:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        // Takes no arguments so "no rules expected the token `,`".
        $crate::__json_unexpected!($comma);
    };

    // Key is fully parenthesized. This avoids clippy double_parens false
    // positives because the parenthesization may be necessary here.
    (@object $object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object ($key) (: $($rest)*) (: $($rest)*));
    };

    // Refuse to absorb colon token into key expression.
    (@object $object:ident ($($key:tt)*) (: $($unexpected:tt)+) $copy:tt) => {
        $crate::__json_expect_expr_comma!($($unexpected)+);
    };

    // Munch a token into the current key.
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::__json!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    //////////////////////////////////////////////////////////////////////////
    // The main implementation.
    //
    // Must be invoked as: __json!($($json)+)
    //////////////////////////////////////////////////////////////////////////

    (null) => {
        $crate::__rt::Value::Null
    };

    (true) => {
        $crate::__rt::Value::Bool(true)
    };

    (false) => {
        $crate::__rt::Value::Bool(false)
    };

    ([]) => {
        $crate::__rt::Value::Array($crate::__rt::vec![])
    };

    ([ $($tt:tt)+ ]) => {
        $crate::__rt::Value::Array($crate::__json!(@array [] $($tt)+))
    };

    ({}) => {
        $crate::__rt::Value::Object($crate::__rt::Map::new())
    };

    ({ $($tt:tt)+ }) => {
        $crate::__rt::Value::Object({
            let mut object = $crate::__rt::Map::new();
            $crate::__json!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };

    // Any Serialize type: numbers, strings, struct literals, variables etc.
    // Must be below every other rule.
    ($other:expr) => {
        $crate::__rt::val(&$other)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json_unexpected {
    () => {};
}

#[macro_export]
#[doc(hidden)]
macro_rules! __json_expect_expr_comma {
    ($e:expr , $($tt:tt)*) => {};
}
