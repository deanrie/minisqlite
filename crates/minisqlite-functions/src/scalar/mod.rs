//! Scalar (row-at-a-time) built-in functions, split into families so each family
//! owns its own file. This hub only declares the families and fans out
//! registration; it holds no function implementations itself.

use std::borrow::Cow;

use minisqlite_expr::FnContext;
use minisqlite_types::{TextEncoding, Value};

use crate::FunctionRegistry;

pub(crate) mod math;
pub(crate) mod misc;
pub(crate) mod string;

/// Register every scalar family into `reg`. Adding a family is a one-line change
/// here plus its own module; a family with nothing implemented yet is a no-op.
/// `args` with every BLOB read as TEXT in the database encoding, for a function that
/// takes its arguments as text (`sqlite3_value_text`): on a UTF-16 database a BLOB's
/// bytes are UTF-16 code units (lang_expr.html "CAST expressions"), so `upper(x'6100')`
/// is `'A'` there. On UTF-8 the arguments are returned as they are, and the functions
/// read a BLOB's bytes as UTF-8 themselves.
pub(crate) fn blobs_as_text<'a>(args: &'a [Value], ctx: &dyn FnContext) -> Cow<'a, [Value]> {
    let enc = ctx.text_encoding();
    if enc == TextEncoding::Utf8 || !args.iter().any(|a| matches!(a, Value::Blob(_))) {
        return Cow::Borrowed(args);
    }
    Cow::Owned(
        args.iter()
            .map(|a| match a {
                Value::Blob(b) => Value::Text(enc.decode_blob(b)),
                other => other.clone(),
            })
            .collect(),
    )
}

pub(crate) fn register(reg: &mut FunctionRegistry) {
    misc::register(reg);
    string::register(reg);
    math::register(reg);
}
