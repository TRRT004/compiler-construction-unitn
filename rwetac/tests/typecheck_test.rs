use insta::assert_snapshot;
mod common;

#[test]
fn binary_wrong_type() {
    let out = common::typecheck_expr("1 + true");
    assert_snapshot!(out, @r#"
    error: invalid operand types for binary `+`
      --> <input>:1:1
      |
    1 | 1 + true
      | ^^^^^^^^ cannot apply `+` to `int` and `bool`
      |
    "#);
}

#[test]
fn binary_wrong_type2() {
    let out = common::typecheck_expr("1 < true");
    assert_snapshot!(out, @r#"
    error: invalid operand types for binary `<`
      --> <input>:1:1
      |
    1 | 1 < true
      | ^^^^^^^^ cannot apply `<` to `int` and `bool`
      |
    "#);
}

// This is a tricky case. I guess both decisions are fine. I decide to allow it {} has type Type::Unknonw and not type Type::Array{Type::Unknown}
#[test]
fn binary_correct_type3() {
    let out = common::typecheck_expr_succeed("{} + {{3, 4}}");
    assert_snapshot!(out, @"int[][]");
}

#[test]
fn binary_correct_type() {
    let out = common::typecheck_expr_succeed("{{1,2}} + {{3, 4}}");
    assert_snapshot!(out, @"int[][]");
}

#[test]
fn binary_correct_type2() {
    let out = common::typecheck_expr_succeed("{} + {3, 4}");
    assert_snapshot!(out, @"int[]");
}
#[test]
fn arr_lit_wrong() {
    let out = common::typecheck_expr("{1, true}");
    assert_snapshot!(out, @r#"
    error: mismatched element types in array literal
      --> <input>:1:5
      |
    1 | {1, true}
      |  -  ^^^^ first element has type `int` here, expected `int`, found `bool`
      |
    "#);
}

#[test]
fn assign_wrong_type() {
    let out = common::typecheck_stmt("x, y = 1 + 3, 4, 5");
    assert_snapshot!(out, @r#"
    error: mismatched number of targets and values in assignment
      --> <input>:1:1
      |
    1 | x, y = 1 + 3, 4, 5
      | ^^^^^^^^^^^^^^^^^^ expected 2 value(s) on the right-hand side, found 3
      |
    "#);
}

#[test]
fn assign_wrong_type2() {
    let out = common::typecheck_stmt("a[2] = 1, 2");
    assert_snapshot!(out, @r#"
    error: mismatched number of targets and values in assignment
      --> <input>:1:1
      |
    1 | a[2] = 1, 2
      | ^^^^^^^^^^^ expected 1 value(s) on the right-hand side, found 2
      |
    "#);
}

#[test]
fn assign_wrong_type3() {
    let out = common::typecheck_stmt("x = test(1,2)");
    assert_snapshot!(out, @r#"
    error: mismatched number of targets and values in assignment
      --> <input>:1:1
      |
    1 | x = test(1,2)
      | ^^^^^^^^^^^^^ expected 1 value(s) on the right-hand side, found 2
      |
    "#);
}

#[test]
fn assign_wrong_type4() {
    let out = common::typecheck_stmt("2 = 6");
    assert_snapshot!(out, @r#"
    error: invalid assignment target
      --> <input>:1:1
      |
    1 | 2 = 6
      | ^ cannot assign to this expression
      |
    "#);
}

#[test]
fn assign_correct_type() {
    let out = common::typecheck_stmt_succeed("x, y, z = test(1,2), 2");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn assign_correct_type2() {
    let _out = common::typecheck_stmt_succeed("_ = 2");
    let out = common::typecheck_stmt_succeed("_ = true");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_assign_correct_type() {
    let out = common::typecheck_stmt_succeed("a[2] = 2");
    assert_snapshot!(out, @"Ok(())");
}
#[test]
fn arr_assign_correct_type2() {
    let out = common::typecheck_stmt_succeed("a = {1,3,4}");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_assign_correct_type2_5() {
    let out = common::typecheck_stmt_succeed("a = {}");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_assign_correct_type3() {
    let out = common::typecheck_stmt_succeed("a = {1,3,4} + {3, 4, 5, 7}");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_assign_correct_type4() {
    let out = common::typecheck_stmt_succeed("a[a[3]] = 4");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_decl_correct_type() {
    let out = common::typecheck_stmt_succeed("g : int[] = {}");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn arr_decl_wrong_type() {
    let out = common::typecheck_stmt("a : int[][4]");
    assert_snapshot!(out, @r#"
    error: invalid array dimensions for `a`
      --> <input>:1:1
      |
    1 | a : int[][4]
      | ^^^^^^^^^^^^ a sized dimension cannot follow an unsized `[]` dimension
      |
    "#);
}

#[test]
fn arr_decl_wrong_type2() {
    let out = common::typecheck_stmt("a : int[4][][5]");
    assert_snapshot!(out, @r#"
    error: invalid array dimensions for `a`
      --> <input>:1:1
      |
    1 | a : int[4][][5]
      | ^^^^^^^^^^^^^^^ a sized dimension cannot follow an unsized `[]` dimension
      |
    "#);
}

#[test]
fn arr_decl_wrong_type3() {
    let out = common::typecheck_stmt("a : int[3] = {1,2}");
    assert_snapshot!(out, @r#"
    error: cannot specify dimensions and initialize array `a`
      --> <input>:1:1
      |
    1 | a : int[3] = {1,2}
      | ^^^^^^^^^^ sized array declaration cannot have an initializer
      |
      = help: remove the explicit size or remove the initializer
    "#);
}

#[test]
fn arr_decl_wrong_type4() {
    let out = common::typecheck_stmt("a : int[true]");
    assert_snapshot!(out, @r#"
    error: array dimensions for `a` must have type `int`
      --> <input>:1:9
      |
    1 | a : int[true]
      |         ^^^^ expected `int`, found `bool`
      |
    "#);
}

#[test]
fn arr_decl_wrong_type5() {
    let out = common::typecheck_stmt("a : int[test(2, 3)]");
    assert_snapshot!(out, @r#"
    error: array dimensions for `a` must have type `int`
      --> <input>:1:9
      |
    1 | a : int[test(2, 3)]
      |         ^^^^^^^^^^ expected `int`, found `(int, int)`
      |
    "#);
}

#[test]
fn arr_decl_wrong_type6() {
    let out = common::typecheck_stmt("b_arr : int[][] = {1,2}");
    assert_snapshot!(out, @r#"
    error: mismatched initializer type for array `b_arr`
      --> <input>:1:19
      |
    1 | b_arr : int[][] = {1,2}
      | ---------------   ^^^^^ expected `int[][]` due to this declaration, expected `int[][]`, found `int[]`
      |
    "#);
}

#[test]
fn length_correct_type() {
    let out = common::typecheck_stmt_succeed("l : int= length(a)");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn length_correct_type2() {
    let out = common::typecheck_stmt_succeed("l : int = length(b)");
    assert_snapshot!(out, @"Ok(())");
}

#[test]
fn if_guard_wrong_type() {
    let out = common::typecheck_stmt("if 2 {}");
    assert_snapshot!(out, @r#"
    error: condition of `if` statement must be `bool`
      --> <input>:1:4
      |
    1 | if 2 {}
      |    ^ expected `bool`, found `int`
      |
      = help: compare the integer to produce a `bool`, e.g. `x != 0`
    "#);
}

#[test]
fn while_guard_wrong_type() {
    let out = common::typecheck_stmt("while 2 {}");
    assert_snapshot!(out, @r#"
    error: condition of while loop must be `bool`
      --> <input>:1:7
      |
    1 | while 2 {}
      |       ^ expected `bool`, found `int`
      |
      = help: compare the integer to produce a `bool`, e.g. `x != 0`
    "#);
}

#[test]
fn multiple_errors_accumulated_in_block() {
    let out = common::typecheck_stmt("{\n    if 2 {}\n    while 3 {}\n    x = true\n}");
    assert_snapshot!(out, @r#"
    error: condition of `if` statement must be `bool`
      --> <input>:2:8
      |
    2 |     if 2 {}
      |        ^ expected `bool`, found `int`
      |
      = help: compare the integer to produce a `bool`, e.g. `x != 0`

    error: condition of while loop must be `bool`
      --> <input>:3:11
      |
    3 |     while 3 {}
      |           ^ expected `bool`, found `int`
      |
      = help: compare the integer to produce a `bool`, e.g. `x != 0`

    error: mismatched types in assignment
      --> <input>:4:9
      |
    4 |     x = true
      |         ^^^^ expected `int`, found `bool`
      |
    "#);
}

#[test]
fn error_type_poisoning_prevents_cascades() {
    let out = common::typecheck_stmt("x = (1 + true) + 5");
    assert_snapshot!(out, @r#"
    error: invalid operand types for binary `+`
      --> <input>:1:5
      |
    1 | x = (1 + true) + 5
      |     ^^^^^^^^^^ cannot apply `+` to `int` and `bool`
      |
    "#);
}
