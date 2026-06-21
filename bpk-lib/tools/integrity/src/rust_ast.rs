#[cfg(test)]
use syn::ExprMethodCall;
use syn::{Expr, ExprCall, ExprPath};

pub(crate) fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

/// Full path for call expressions: `std::fs::write`, `fs::metadata`, `Store::open`.
pub(crate) fn callee_path_segments(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Path(ExprPath { path, .. }) => Some(path_segments(path)),
        Expr::Call(ExprCall { func, .. }) => callee_path_segments(func),
        Expr::Array(_)
        | Expr::Assign(_)
        | Expr::Async(_)
        | Expr::Await(_)
        | Expr::Binary(_)
        | Expr::Block(_)
        | Expr::Break(_)
        | Expr::Cast(_)
        | Expr::Closure(_)
        | Expr::Const(_)
        | Expr::Continue(_)
        | Expr::Field(_)
        | Expr::ForLoop(_)
        | Expr::Group(_)
        | Expr::If(_)
        | Expr::Index(_)
        | Expr::Infer(_)
        | Expr::Let(_)
        | Expr::Lit(_)
        | Expr::Loop(_)
        | Expr::Macro(_)
        | Expr::Match(_)
        | Expr::MethodCall(_)
        | Expr::Paren(_)
        | Expr::Range(_)
        | Expr::RawAddr(_)
        | Expr::Reference(_)
        | Expr::Repeat(_)
        | Expr::Return(_)
        | Expr::Struct(_)
        | Expr::Try(_)
        | Expr::TryBlock(_)
        | Expr::Tuple(_)
        | Expr::Unary(_)
        | Expr::Unsafe(_)
        | Expr::Verbatim(_)
        | Expr::While(_)
        | Expr::Yield(_) => None,
        // `syn::Expr` is `#[non_exhaustive]`; future variants are non-call,
        // non-path expressions with no callee path to extract.
        _ => None,
    }
}

/// Last two path segments as `(owner, method)`, matching platform-boundary logic.
pub(crate) fn tail_owner_method(segments: &[String]) -> Option<(&str, &str)> {
    match segments {
        [owner, method] => Some((owner.as_str(), method.as_str())),
        [.., owner, method] => Some((owner.as_str(), method.as_str())),
        _ => None,
    }
}

/// Method-call shape: receiver path segments + method ident.
#[cfg(test)]
pub(crate) fn method_call_segments(call: &ExprMethodCall) -> Option<(Vec<String>, String)> {
    let method = call.method.to_string();
    let receiver_segments = callee_path_segments(&call.receiver)?;
    Some((receiver_segments, method))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn callee_path_segments_qualified_fs_write() {
        let expr: Expr = parse_quote!(std::fs::write(path, bytes));
        let segments = callee_path_segments(&expr).expect("call path");
        assert_eq!(segments, vec!["std", "fs", "write"]);
    }

    #[test]
    fn callee_path_segments_imported_metadata() {
        let expr: Expr = parse_quote!(fs::metadata(path));
        let segments = callee_path_segments(&expr).expect("call path");
        assert_eq!(segments, vec!["fs", "metadata"]);
    }

    #[test]
    fn tail_owner_method_handles_two_and_multi_segment_paths() {
        assert_eq!(
            tail_owner_method(&["fs".to_string(), "metadata".to_string()]),
            Some(("fs", "metadata"))
        );
        assert_eq!(
            tail_owner_method(&["std".to_string(), "fs".to_string(), "write".to_string()]),
            Some(("fs", "write"))
        );
        assert_eq!(tail_owner_method(&["only".to_string()]), None);
    }

    #[test]
    fn method_call_segments_extracts_receiver_and_method() {
        let call: ExprMethodCall = parse_quote!(options.map(file));
        let (receiver, method) = method_call_segments(&call).expect("method call");
        assert_eq!(receiver, vec!["options"]);
        assert_eq!(method, "map");
    }
}
