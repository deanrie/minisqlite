//! Conformance battery for the paths `json_set`, `json_insert`, `json_replace` and
//! `json_remove` edit (json1.html §4.11, §4.18) when part of the path does not exist.
//!
//! json1.html does not spell out what happens to a missing intermediate container
//! or an array index past the end, so every expectation here is the output of
//! sqlite3 3.51.3 for the same expression. In short: `json_set` and `json_insert`
//! create the missing rest of the path (objects for labels, arrays for `[0]`, `[#]`
//! or `[#-0]`), an existing array only grows at the index one past its end, and
//! `json_replace` / `json_remove` never create anything.
//!
//! JSON SQL is quote-heavy, so SQL and expected JSON are Rust raw strings.

mod conformance;

use conformance::*;

#[test]
fn json_set_creates_missing_objects() {
    // The four rows from #13 section 3, and deeper paths.
    eval_eq(r#"json_set('{}', '$.a.b', 1)"#, text(r#"{"a":{"b":1}}"#));
    eval_eq(r#"json_set('{"a":{}}', '$.a.b.c', 1)"#, text(r#"{"a":{"b":{"c":1}}}"#));
    eval_eq(r#"json_insert('{}', '$.x.y', 2)"#, text(r#"{"x":{"y":2}}"#));
    eval_eq(r#"json_set('[]', '$[0].k', 1)"#, text(r#"[{"k":1}]"#));
    eval_eq(r#"json_set('{}', '$.a.b.c.d', 1)"#, text(r#"{"a":{"b":{"c":{"d":1}}}}"#));
    eval_eq(r#"json_set('{"a":{"b":1}}', '$.a.c.d', 2)"#, text(r#"{"a":{"b":1,"c":{"d":2}}}"#));
    eval_eq(r#"json_insert('{"a":{"b":1}}', '$.a.c.d', 2)"#, text(r#"{"a":{"b":1,"c":{"d":2}}}"#));
    eval_eq(r#"json_set('{}', '$."x y".z', 1)"#, text(r#"{"x y":{"z":1}}"#));
    eval_eq(r#"json_set('{"a":{},"a":{"x":1}}', '$.a.b', 3)"#, text(r#"{"a":{"b":3},"a":{"x":1}}"#));
}

#[test]
fn json_set_creates_missing_arrays_only_at_index_zero_or_append() {
    // A missing array is created for [0], [#] or [#-0]; any other index creates nothing,
    // not even the objects before it.
    eval_eq(r#"json_set('{}', '$.a[0]', 1)"#, text(r#"{"a":[1]}"#));
    eval_eq(r#"json_set('{}', '$.a[#]', 1)"#, text(r#"{"a":[1]}"#));
    eval_eq(r#"json_set('{}', '$.a[#-0]', 1)"#, text(r#"{"a":[1]}"#));
    eval_eq(r#"json_set('[]', '$[#].k', 1)"#, text(r#"[{"k":1}]"#));
    eval_eq(r#"json_set('[]', '$[0][0][0]', 1)"#, text(r#"[[[1]]]"#));
    eval_eq(r#"json_set('{}', '$.a[0].b[0]', 1)"#, text(r#"{"a":[{"b":[1]}]}"#));
    eval_eq(r#"json_insert('[]', '$[#][#]', 1)"#, text(r#"[[1]]"#));
    eval_eq(r#"json_set('{}', '$.a[1]', 1)"#, text(r#"{}"#));
    eval_eq(r#"json_set('{}', '$.a[#-1]', 1)"#, text(r#"{}"#));
    eval_eq(r#"json_set('{}', '$.a[#-1].b', 1)"#, text(r#"{}"#));
    eval_eq(r#"json_set('{}', '$.a.b[1]', 1)"#, text(r#"{}"#));
    eval_eq(r#"json_insert('[]', '$[0][1]', 1)"#, text(r#"[]"#));
}

#[test]
fn json_set_index_past_the_end_changes_nothing() {
    // Only the index one past the end (or [#], [#-0]) appends; a larger index is a no-op.
    eval_eq(r#"json_set('[1,2]', '$[5]', 9)"#, text(r#"[1,2]"#));
    eval_eq(r#"json_insert('[]', '$[1]', 1)"#, text(r#"[]"#));
    eval_eq(r#"json_set('[1,2,3]', '$[4]', 4)"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_insert('[1,2,3]', '$[100]', 4)"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_set('[[1]]', '$[0][2]', 2)"#, text(r#"[[1]]"#));
    eval_eq(r#"json_set('[[1]]', '$[2][0]', 2)"#, text(r#"[[1]]"#));
    eval_eq(r#"json_set('{"a":[1]}', '$.a[2].b', 2)"#, text(r#"{"a":[1]}"#));
    eval_eq(r#"json_set('[1,2]', '$[2]', 9)"#, text(r#"[1,2,9]"#));
    eval_eq(r#"json_insert('[1,2,3]', '$[3]', 4)"#, text(r#"[1,2,3,4]"#));
    eval_eq(r#"json_set('[1]', '$[#-0]', 5)"#, text(r#"[1,5]"#));
    eval_eq(r#"json_insert('[1,2,3]', '$[#-0]', 4)"#, text(r#"[1,2,3,4]"#));
    eval_eq(r#"json_set('[[1]]', '$[0][1]', 2)"#, text(r#"[[1,2]]"#));
    eval_eq(r#"json_set('[[1]]', '$[1][0]', 2)"#, text(r#"[[1],[2]]"#));
    eval_eq(r#"json_set('{"a":[1]}', '$.a[1].b', 2)"#, text(r#"{"a":[1,{"b":2}]}"#));
    eval_eq(r#"json_set('[[1,2],[3]]', '$[#-1][#]', 4)"#, text(r#"[[1,2],[3,4]]"#));
    eval_eq(r#"json_set('[[1,2],[3]]', '$[#][1]', 4)"#, text(r#"[[1,2],[3]]"#));
}

#[test]
fn json_set_from_the_end_index_never_creates() {
    // [#-N] with N > 0 only selects existing elements.
    eval_eq(r#"json_set('[1]', '$[#-1]', 5)"#, text(r#"[5]"#));
    eval_eq(r#"json_set('[1]', '$[#-2]', 5)"#, text(r#"[1]"#));
    eval_eq(r#"json_set('[]', '$[#-1]', 5)"#, text(r#"[]"#));
    eval_eq(r#"json_insert('[1,2,3]', '$[#-1]', 4)"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_set('{"a":[{"b":1}]}', '$.a[#-1].c.d', 2)"#, text(r#"{"a":[{"b":1,"c":{"d":2}}]}"#));
}

#[test]
fn json_set_type_mismatch_changes_nothing() {
    // A label on a non-object or an index on a non-array is a no-op, also when the rest
    // of the path is missing.
    eval_eq(r#"json_set('{"a":1}', '$.a.b', 2)"#, text(r#"{"a":1}"#));
    eval_eq(r#"json_set('{"a":null}', '$.a.b', 1)"#, text(r#"{"a":null}"#));
    eval_eq(r#"json_set('{"a":"x"}', '$.a[0]', 1)"#, text(r#"{"a":"x"}"#));
    eval_eq(r#"json_set('{"a":[]}', '$.a.b', 1)"#, text(r#"{"a":[]}"#));
    eval_eq(r#"json_set('{"a":{}}', '$.a[0]', 1)"#, text(r#"{"a":{}}"#));
    eval_eq(r#"json_set('"s"', '$.a.b', 1)"#, text(r#""s""#));
    eval_eq(r#"json_set('1', '$[0]', 1)"#, text(r#"1"#));
    eval_eq(r#"json_set('[1]', '$.a.b', 1)"#, text(r#"[1]"#));
}

#[test]
fn json_replace_and_remove_never_create() {
    // json_replace and json_remove leave missing paths alone, intermediate or final.
    eval_eq(r#"json_replace('{}', '$.a.b', 1)"#, text(r#"{}"#));
    eval_eq(r#"json_replace('{"a":{"b":1}}', '$.a.c.d', 2)"#, text(r#"{"a":{"b":1}}"#));
    eval_eq(r#"json_replace('[1,2]', '$[2]', 9)"#, text(r#"[1,2]"#));
    eval_eq(r#"json_replace('[1,2,3]', '$[#-0]', 4)"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_replace('{"a":{"b":[1,{"c":2}]}}', '$.a.b[1].c', 3)"#, text(r#"{"a":{"b":[1,{"c":3}]}}"#));
    eval_eq(r#"json_remove('{}', '$.a.b')"#, text(r#"{}"#));
    eval_eq(r#"json_remove('[1,2,3]', '$[3]')"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_remove('[1,2,3]', '$[#-0]')"#, text(r#"[1,2,3]"#));
    eval_eq(r#"json_remove('{"a":{"b":1}}', '$.a.b.c')"#, text(r#"{"a":{"b":1}}"#));
}

#[test]
fn json_set_several_pairs_see_earlier_edits() {
    // Pairs apply left to right, so a later path can go through a container an earlier
    // pair created.
    eval_eq(r#"json_set('{}', '$.a.b', 1, '$.a.c', 2)"#, text(r#"{"a":{"b":1,"c":2}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', 1, '$.a.b.c', 2)"#, text(r#"{"a":{"b":1}}"#));
    eval_eq(r#"json_set('{}', '$.a', 1, '$.a.b', 2)"#, text(r#"{"a":1}"#));
    eval_eq(r#"json_insert('{}', '$.a.b', 1, '$.a.b', 2)"#, text(r#"{"a":{"b":1}}"#));
    eval_eq(r#"json_set('{}', '$.a[0]', 1, '$.a[1]', 2, '$.a[3]', 3)"#, text(r#"{"a":[1,2]}"#));
    eval_eq(r#"json_set('{}', '$.a[0]', 1, '$.a[#]', 2, '$.a[#-0]', 3)"#, text(r#"{"a":[1,2,3]}"#));
    eval_eq(r#"json_set('[]', '$[0]', 1, '$[0]', 2, '$[1]', 3, '$[3]', 4)"#, text(r#"[2,3]"#));
    eval_eq(r#"json_set('{}', NULL, 1, '$.a.b', 2)"#, text(r#"{"a":{"b":2}}"#));
    eval_eq(r#"json_set('[1,2]', '$[3]', 9, '$[2]', 8)"#, text(r#"[1,2,8]"#));
}

#[test]
fn json_set_created_path_holds_any_value() {
    // The new leaf takes the value as json_set would store it at an existing path.
    eval_eq(r#"json_set('{}', '$.a.b', NULL)"#, text(r#"{"a":{"b":null}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', 'x')"#, text(r#"{"a":{"b":"x"}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', 1.5)"#, text(r#"{"a":{"b":1.5}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', '[1]')"#, text(r#"{"a":{"b":"[1]"}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', json('[1]'))"#, text(r#"{"a":{"b":[1]}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', json_object('c', 3))"#, text(r#"{"a":{"b":{"c":3}}}"#));
    eval_eq(r#"json_set('{}', '$.a.b', json_set('{}', '$.c.d', 1))"#, text(r#"{"a":{"b":{"c":{"d":1}}}}"#));
    eval_eq(r#"json_type(json_set('{}', '$.a.b', 1), '$.a')"#, text(r#"object"#));
    eval_eq(r#"json_set(json_set('{}', '$.a.b', 1), '$.a.c', 2)"#, text(r#"{"a":{"b":1,"c":2}}"#));
}

#[test]
fn json_set_creates_a_deep_path() {
    // 1000 missing labels in one path.
    eval_eq(r#"length(json_set('{}', '$' || replace(printf('%.1000c', 'x'), 'x', '.a'), 1))"#, int(6001));
    eval_eq(r#"json_extract(json_set('{}', '$' || replace(printf('%.1000c', 'x'), 'x', '.a'), 1), '$' || replace(printf('%.1000c', 'x'), 'x', '.a'))"#, int(1));
}
