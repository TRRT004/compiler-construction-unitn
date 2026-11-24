use insta::assert_snapshot;
use rwetac::types;
mod common;

#[test]
fn record_not_mem() {
    let out = common::typecheck_stmt("a : int = p.z");
    assert_snapshot!(out, @r#"
    error: unknown field `z` on record `Point`
      --> <input>:1:11
      |
    1 | a : int = p.z
      |           ^^^ record `Point` has no field named `z`
      |
      = help: available fields are: `x`, `y`
    "#);
}

#[test]
fn record_not_mem2() {
    let out = common::typecheck_stmt("a : int = p.x.z");
    assert_snapshot!(out, @r#"
    error: cannot access field `z` on non-record type `int`
      --> <input>:1:11
      |
    1 | a : int = p.x.z
      |           ^^^ expected a record type, found `int`
      |
    "#);
}

// #[test]
// fn record_undef() {
//     let out = expect_failure(|| {
//         let ast = parse_stmt("p : Point5D");
//         // simulate resolve + typecheck
//         let _resolved = resolve_stmt(&ast);
//         typecheck_stmt(&ast)
//     });
//     assert_snapshot!(out, @r###"
//     Failure: Struct type is not defined
//     "###);
// }

#[test]
fn record_wrong_init() {
    let out = common::typecheck_stmt("p : Point = Point()");
    assert_snapshot!(out, @r#"
    error: wrong number of arguments to record `Point`
      --> <input>:1:13
      |
    1 | p : Point = Point()
      |             ^^^^^^^ expected 2 argument(s), found 0
      |
    "#);
}
#[test]
fn record_correct_init2() {
    let out = common::typecheck_stmt_succeed("p : Point = Point(2, 3)");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn record_wrong_assign() {
    let out = common::typecheck_stmt("p.x = true");
    assert_snapshot!(out, @r#"
    error: mismatched types in assignment
      --> <input>:1:7
      |
    1 | p.x = true
      |       ^^^^ expected `int`, found `bool`
      |
    "#);
}

// #[test]
// fn record_dupl_member() {
//     let out = expect_failure(|| {
//         let ast = parse_stmt("record Point3 { x : int; x : int }");
//         let _resolved = resolve_stmt(&ast);
//         typecheck_stmt(&ast)
//     });
//     assert_snapshot!(out, @r###"
//     Failure: Member already declared in record
//     "###);
// }

#[test]
fn record_correct_assign() {
    let mut env = common::setup("p.x = 2");
    // This one should succeed, so we don’t wrap it in expect_failure.
    let ast = env.parser.parse_statement();
    assert!(
        env.parser.errors().is_empty(),
        "unexpected parse errors: {:?}",
        env.parser.errors()
    );
    let result = env.tchecker.typecheck_statement(&ast, &types::Type::Bool);
    assert!(env.tchecker.diagnostics.is_empty());
    assert_snapshot!(format!("{result:?}"), @"Unit");
}
