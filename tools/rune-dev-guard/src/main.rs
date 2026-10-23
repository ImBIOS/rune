//! A syntax guard, not a size_of guess: same-size field/type/signature edits are incompatible too.
use quote::ToTokens;
use syn::{parse::Parser, visit::Visit, visit_mut::VisitMut};

#[derive(Default)]
struct Contracts(Vec<String>);
impl<'ast> Visit<'ast> for Contracts {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        // Local types, statics, functions and imports are also contracts. Do not erase them.
        self.0.push(item.to_token_stream().to_string());
    }
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        // Unknown macros could expand to persistent types/statics. Keep their entire invocation.
        let safe = ["format", "format_args", "vec", "matches", "println", "eprintln", "assert", "assert_eq", "debug_assert", "panic", "unreachable", "todo"];
        if m.path.get_ident().is_some_and(|i| safe.iter().any(|s| i == s)) {
            // syn does not visit macro tokens. Parse their expressions explicitly so a
            // local type/static or nested unknown macro cannot hide inside vec!/format!.
            if m.path.is_ident("vec") {
                let tokens = &m.tokens;
                if let Ok(expr) = syn::parse2::<syn::Expr>(quote::quote!([#tokens])) {
                    self.visit_expr(&expr);
                    return;
                }
            } else if let Ok(args) = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated.parse2(m.tokens.clone()) {
                for arg in args { self.visit_expr(&arg); }
                return;
            }
        }
        self.0.push(m.to_token_stream().to_string());
    }
}

#[derive(Default)]
struct Scrub(Contracts);
impl VisitMut for Scrub {
    fn visit_item_fn_mut(&mut self, f: &mut syn::ItemFn) {
        // Const bodies can determine persistent layouts. Unknown attributes can
        // expand the body into types or statics, even on an ordinary function.
        let inert = ["doc", "inline", "cold", "allow"];
        if f.sig.ident == "main" || f.sig.constness.is_some()
            || f.attrs.iter().any(|a| !a.path().get_ident().is_some_and(|i| inert.iter().any(|s| i == s)))
        { return; }
        self.0.visit_block(&f.block);
        f.block = syn::parse_quote!({});
    }
    // Impl methods, initializers, constants, attributes and module declarations stay byte-token exact.
    fn visit_impl_item_fn_mut(&mut self, _: &mut syn::ImplItemFn) {}
}

fn contract(src: &str) -> Result<String, syn::Error> {
    let mut ast = syn::parse_file(src)?;
    let mut scrub = Scrub::default();
    scrub.visit_file_mut(&mut ast);
    Ok(format!("{}\n{:?}", ast.to_token_stream(), scrub.0.0))
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2, "rune-dev-guard BEFORE.rs AFTER.rs");
    let read = |p: &str| contract(&std::fs::read_to_string(p).expect("read source"));
    match (read(&args[0]), read(&args[1])) {
        (Ok(a), Ok(b)) if a == b => {}
        (Ok(_), Ok(_)) => { eprintln!("restart needed: declaration, state schema, signature, startup code or persistent macro changed"); std::process::exit(3); }
        (_, Err(e)) | (Err(e), _) => { eprintln!("edit not ready: {e}"); std::process::exit(2); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatible_bodies_but_not_state_or_boundary_changes() {
        let base = "struct S { a: u32, b: u32 } fn view(s: &S) -> u32 { s.a + 1 } fn main() { let s = S { a: 1, b: 2 }; }";
        assert_eq!(contract(base).unwrap(), contract(&base.replace("s.a + 1", "s.b + 20")).unwrap());
        for edit in [base.replace("a: u32, b: u32", "b: u32, a: u32"), base.replace("s: &S", "s: &mut S"), base.replace("a: 1", "a: 9"), base.replace("s.a + 1", "static X: u32 = 3; s.a + X"), base.replace("s.a + 1", "mystery!(s)")] {
            assert_ne!(contract(base).unwrap(), contract(&edit).unwrap());
        }
    }
    #[test]
    fn nested_types_and_impls_cannot_change() {
        let base = "fn view() { struct S { x: u32 } } impl S { fn init() -> u32 { 1 } }";
        assert_ne!(contract(base).unwrap(), contract(&base.replace("x: u32", "x: f32")).unwrap());
        assert_ne!(contract(base).unwrap(), contract(&base.replace("{ 1 }", "{ 2 }")).unwrap());
        assert!(contract("fn view( {").is_err());
    }

    #[test]
    fn const_function_bodies_are_layout_contracts() {
        for layout in ["struct S { buf: [u8; cap()] }", "const N: usize = cap(); struct S { buf: [u8; N] }"] {
            let base = format!("const fn cap() -> usize {{ 8 }} {layout} fn view() {{}}");
            assert_ne!(contract(&base).unwrap(), contract(&base.replace("{ 8 }", "{ 16 }")).unwrap());
        }
    }

    #[test]
    fn unknown_attributes_keep_bodies_but_inert_attributes_allow_edits() {
        for attribute in ["#[persistent]", "#[some::macro_attr]", "#[cfg_attr(feature = \"x\", persistent)]"] {
            let base = format!("{attribute} fn view() {{ let n = 1; }}");
            assert_ne!(contract(&base).unwrap(), contract(&base.replace("n = 1", "n = 2")).unwrap());
        }
        let base = "#[doc = \"view\"] #[inline(never)] #[cold] #[allow(unused)] fn view() { let n = 1; }";
        assert_eq!(contract(base).unwrap(), contract(&base.replace("n = 1", "n = 2")).unwrap());
        assert_ne!(contract(base).unwrap(), contract(&base.replace("inline(never)", "inline(always)")).unwrap());
    }

    #[test]
    fn macro_arguments_cannot_hide_persistent_contracts() {
        for body in ["vec![{ static X: u32 = 1; X }]", "format!(\"{}\", { struct S { x: u32 } 1 })", "vec![unknown!(1)]"] {
            let base = format!("fn view() {{ {body}; }}");
            assert_ne!(contract(&base).unwrap(), contract(&base.replace("u32", "f32").replace("unknown!(1)", "unknown!(2)")).unwrap());
        }
        assert_eq!(contract("fn view() { vec![1, 2]; }").unwrap(), contract("fn view() { vec![3; 4]; }").unwrap());
    }
}
