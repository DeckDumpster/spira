//! `#[test_id(<id>)]` rewrites `#[test_id(t7k3m9q)] #[test] fn name() { … }` to
//! `mod name { use super::*; #[test] fn t7k3m9q() { … } }`, so the test is listed as
//! `…::name::t7k3m9q`: the name stays for people, the last segment is the id.
//!
//! The id is stable with the test's intent, not its name or its code. Rename or move a test
//! and keep its id. A test rewritten to check something else is a new test: delete the
//! attribute and run `test-registry stamp`. Nobody types an id; `test-registry mint` prints one.
//!
//! A body naming `super::` resolves one module higher once wrapped; the compiler names each
//! such site.

use proc_macro::{Delimiter, Group, Ident, Span, TokenStream, TokenTree};

mod id;
use id::{check_id, GRAMMAR};

fn compile_error(msg: &str) -> TokenStream {
    format!("compile_error!({msg:?});").parse().expect("compile_error tokens")
}

fn expand(args: TokenStream, item: TokenStream) -> Result<TokenStream, String> {
    let mut a = args.into_iter();
    let id = match (a.next(), a.next()) {
        (Some(TokenTree::Ident(i)), None) => i.to_string(),
        _ => return Err(format!("#[test_id] takes exactly one identifier: {GRAMMAR}")),
    };
    check_id(&id)?;
    let tokens: Vec<TokenTree> = item.into_iter().collect();
    let kw = tokens
        .iter()
        .position(|t| matches!(t, TokenTree::Ident(i) if i.to_string() == "fn"))
        .ok_or("#[test_id] applies to a fn")?;
    let name = match tokens.get(kw + 1) {
        Some(TokenTree::Ident(n)) => n.to_string(),
        _ => return Err("#[test_id] applies to a fn".into()),
    };
    let mut out: Vec<TokenTree> = Vec::new();
    for (i, t) in tokens.into_iter().enumerate() {
        if i == kw + 1 {
            out.push(TokenTree::Ident(Ident::new(&id, Span::call_site())));
        } else {
            out.push(t);
        }
    }
    let inner: TokenStream = out.into_iter().collect();
    let head: TokenStream = format!("mod {name} {{ use super::*; }}").parse().map_err(|_| "bad fn name".to_string())?;
    let mut res: Vec<TokenTree> = head.into_iter().collect();
    let Some(TokenTree::Group(_)) = res.pop() else { return Err("internal: mod body".into()) };
    res.push(TokenTree::Group(Group::new(Delimiter::Brace, {
        let mut body: TokenStream = "use super::*;".parse().expect("use tokens");
        body.extend(inner);
        body
    })));
    Ok(res.into_iter().collect())
}

#[proc_macro_attribute]
pub fn test_id(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(args, item).unwrap_or_else(|e| compile_error(&e))
}
