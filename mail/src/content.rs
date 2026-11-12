//! [`ContentBd`]: the bead reads `tidy` makes (`list`, `show`), over the store's `content`
//! verb on [`LcClient`] — in-process, no `bdq` child. It reads only: a close, an ask or any
//! write is refused, because the content door serves none of them.

use spira_store::lc_call::{LcClient, LcError};

use crate::bead::{Bd, BdOut};

pub struct ContentBd<'a> {
    pub lc: &'a LcClient,
}

impl Bd for ContentBd<'_> {
    fn run(&self, args: &[String], stdin: Option<&str>) -> BdOut {
        if stdin.is_some() {
            return BdOut::fail(3, "the content door takes no request body");
        }
        let argv: Vec<&str> = std::iter::once("content").chain(args.iter().map(String::as_str)).collect();
        match self.lc.call(&argv) {
            Ok(r) => BdOut { code: r.code, stdout: r.out, stderr: r.err },
            Err(e) => BdOut::fail(spira_store::lc_call::CANNOT_TELL, e.to_string()),
        }
    }

    fn close(&self, id: &str, _: &str) -> Result<(), LcError> {
        Err(LcError::Refused(format!("ContentBd reads only; refusing to close {id}")))
    }

    fn ensure_ask(&self, id: &str, _: &str) -> Result<(), String> {
        Err(format!("ContentBd reads only; refusing to open ask {id}"))
    }
}
