#![forbid(unsafe_code)]
//! Internal proc-macro helpers for kagari — **no `syn`/`quote`**, hand-written `TokenStream` walks.
//!
//! Currently one macro: [`idents!`], a minimal identifier-concatenation pass that replaces the
//! unmaintained `paste` crate for kagari-style's `Styled` setters. It scans its body for `[< … >]`
//! groups and replaces each with the single identifier formed by concatenating the inner
//! identifiers — e.g. `[<gap _2>]` → `gap_2`. This is the one piece of macro magic kagari's public
//! styling API depends on, so it is owned in-house rather than left on an archived dependency.
//!
//! Scope is deliberately narrow: it concatenates **identifiers only** (the kagari use sites pass
//! idents like `gap` + `_0p5`); it does not implement `paste`'s literal/case-conversion features.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

/// Replaces every `[< a b … >]` marker in the input with the identifier `ab…` (inner identifiers
/// concatenated). Non-marker tokens — including ordinary `[...]` brackets — pass through unchanged;
/// nested groups are processed recursively.
#[proc_macro]
pub fn idents(input: TokenStream) -> TokenStream {
    expand(input)
}

fn expand(input: TokenStream) -> TokenStream {
    let mut out: Vec<TokenTree> = Vec::new();
    for tt in input {
        match tt {
            TokenTree::Group(group) => {
                // A `[< … >]` concat marker is a bracket group whose contents start with `<` and
                // end with `>`; anything else (including a real `[...]`) recurses untouched.
                if group.delimiter() == Delimiter::Bracket {
                    if let Some(ident) = try_concat(&group) {
                        out.push(TokenTree::Ident(ident));
                        continue;
                    }
                }
                out.push(TokenTree::Group(Group::new(
                    group.delimiter(),
                    expand(group.stream()),
                )));
            }
            other => out.push(other),
        }
    }
    out.into_iter().collect()
}

/// If `group` is `[< tok tok … >]` with identifier contents, returns the concatenated identifier;
/// otherwise `None` (the caller then treats the group as ordinary tokens).
fn try_concat(group: &Group) -> Option<Ident> {
    let toks: Vec<TokenTree> = group.stream().into_iter().collect();
    if !is_punct(toks.first()?, '<') || !is_punct(toks.last()?, '>') {
        return None;
    }
    let mut name = String::new();
    let mut span: Option<Span> = None;
    for tok in &toks[1..toks.len() - 1] {
        match tok {
            TokenTree::Ident(id) => {
                span.get_or_insert_with(|| id.span());
                name.push_str(&id.to_string());
            }
            // Identifiers only — reject anything else so a stray bracket is left untouched.
            _ => return None,
        }
    }
    if name.is_empty() {
        return None;
    }
    Some(Ident::new(&name, span.unwrap_or_else(Span::call_site)))
}

fn is_punct(tok: &TokenTree, ch: char) -> bool {
    matches!(tok, TokenTree::Punct(p) if p.as_char() == ch)
}
