//! Small text helpers for reading expressions out of source.

use std::collections::HashSet;

use crate::model::NodeId;

/// The single element of `set`, or None if it is empty or has several.
pub(super) fn only_one(set: &HashSet<NodeId>) -> Option<NodeId> {
    let mut it = set.iter();
    match (it.next(), it.next()) {
        (Some(one), None) => Some(*one),
        _ => None,
    }
}

pub(super) fn is_dotted_ident(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            let mut c = p.chars();
            matches!(c.next(), Some(ch) if ch == '_' || ch.is_alphabetic())
                && c.all(|ch| ch == '_' || ch.is_alphanumeric())
        })
}

/// For `a.b.C(args)`, return `a.b.C` when the whole text is exactly one call.
pub(super) fn single_call_callee(s: &str) -> Option<&str> {
    let open = s.find('(')?;
    let head = &s[..open];
    if !s.ends_with(')') || !is_dotted_ident(head) {
        return None;
    }
    // The paren opened at `open` must be the one closed by the last char.
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for (i, ch) in s[open..].char_indices() {
        match quote {
            Some(q) => {
                if ch == q && prev != '\\' {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (open + i == s.len() - 1).then_some(head);
                    }
                }
                _ => {}
            },
        }
        prev = ch;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_call_receivers() {
        assert_eq!(
            single_call_callee("requests.Request(\"GET\", url)"),
            Some("requests.Request")
        );
        assert_eq!(single_call_callee("Foo(bar(1), \")\")"), Some("Foo"));
        assert_eq!(single_call_callee("foo(1)(2)"), None);
        assert_eq!(single_call_callee("foo(1).x"), None);
        assert_eq!(single_call_callee("a[0](1)"), None);
    }
}
