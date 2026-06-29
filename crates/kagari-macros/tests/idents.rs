//! Integration test for the `idents!` concat macro (a proc-macro can only be exercised from a
//! separate crate). Verifies `[< a b >]` collapses to the single identifier `ab`.

use kagari_macros::idents;

idents! {
    fn [<concatenated _name>]() -> u32 {
        42
    }
}

#[test]
fn idents_should_concatenate_into_one_identifier() {
    assert_eq!(concatenated_name(), 42);
}

idents! {
    fn [<three _part _ident>]() -> u32 {
        7
    }
}

#[test]
fn idents_should_concatenate_three_parts() {
    assert_eq!(three_part_ident(), 7);
}
