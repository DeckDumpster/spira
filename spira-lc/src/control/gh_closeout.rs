//! `gh-closeout pending|show|done|asked|failed`: the pending GitHub closeouts the drain works
//! through, on the control store. JSON on every path; exit 2 ("cannot tell") with the reason.

use serde_json::{json, Value};
use state_store::{Backend, Request};

use super::store::Store;
use crate::CANNOT_TELL;

pub const VERB: &str = "gh-closeout";

pub fn run<B: Backend>(store: &Store<B>, args: &[String], request: &str) -> (i32, String) {
    match answer(store, args, request) {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn answer<B: Backend>(store: &Store<B>, args: &[String], request: &str) -> Result<String, String> {
    let req = Request::new(request, "gh-closeout", "gh-closeout");
    let fail = |e: state_store::Error| format!("gh-closeout: {e}");
    let bead = || args.get(2).map(String::as_str).filter(|b| !b.is_empty()).ok_or("gh-closeout: missing <bead>");
    match args.get(1).map(String::as_str) {
        Some("pending") => {
            let rows: Vec<Value> = store
                .gh_closeouts()
                .map_err(fail)?
                .into_iter()
                .map(|(id, mut row)| {
                    row["bead_id"] = Value::String(id);
                    row
                })
                .collect();
            Ok(Value::Array(rows).to_string())
        }
        Some("show") => {
            let id = bead()?;
            Ok(store.gh_closeout(id).map_err(fail)?.map_or(json!({}), |mut r| {
                r["bead_id"] = Value::String(id.to_string());
                r
            }).to_string())
        }
        Some("done") => {
            store.gh_closeout_done(bead()?, &req).map_err(fail)?;
            Ok(json!({ "done": bead()? }).to_string())
        }
        Some("asked") => {
            let ask = args.get(3).map(String::as_str).filter(|a| !a.is_empty()).ok_or("gh-closeout: asked needs <bead> <ask-id>")?;
            let found = store.gh_closeout_asked(bead()?, ask, &req).map_err(fail)?;
            Ok(json!({ "asked": bead()?, "pending": found }).to_string())
        }
        Some("failed") => {
            let found = store.gh_closeout_failed(bead()?, &req).map_err(fail)?;
            Ok(json!({ "failed": bead()?, "pending": found }).to_string())
        }
        other => Err(format!("gh-closeout: {other:?} is not pending, show, done, asked or failed")),
    }
}
