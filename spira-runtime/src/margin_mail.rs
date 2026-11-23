//! The mail one comment of the operator's in the Margin reader becomes: its collection, the
//! document's path and anchored lines, the source around them, the thread so far and the command
//! that answers it.

use std::path::Path;

use margin_client::Margin;

use crate::sources::smtp::Delivery;
use crate::watchers::iso8601;

pub const MAILBOX: &str = "concierge";
const CONTEXT: u32 = 5;

/// A reader's text on one header line: a control character, a newline above all, is a space.
fn header_text(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn id_part(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .collect()
}

pub fn message_id(comment: &str) -> String {
    format!("<margin-comment-{}@spira>", id_part(comment))
}

fn source_block(text: &str, first: u32, last: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let (from, to) = (
        first.saturating_sub(CONTEXT).max(1),
        (last + CONTEXT).min(lines.len() as u32),
    );
    (from..=to)
        .map(|n| {
            format!(
                "{}{n:>5}  {}\n",
                if (first..=last).contains(&n) {
                    '>'
                } else {
                    ' '
                },
                lines[(n - 1) as usize]
            )
        })
        .collect()
}

/// The message for `comment` of `thread` in `collection`, read through `margin`.
pub fn build(
    margin: &dyn Margin,
    base: &str,
    run_dir: &Path,
    collection: &str,
    thread: &str,
    comment: &str,
) -> Result<String, String> {
    let info = margin.collection(collection)?;
    let t = margin.thread(collection, thread)?;
    let at = t
        .comments
        .iter()
        .position(|c| c.id == comment)
        .ok_or_else(|| format!("thread {thread} holds no comment {comment}"))?;
    let doc = &t.document;
    let text = margin.document_text(&doc.id, doc.version)?;
    let (first, last) = (t.anchor.first, t.anchor.last.max(t.anchor.first));
    let range = if last > first {
        format!("{first}-{last}")
    } else {
        first.to_string()
    };
    let review = cr::record::all(run_dir)?
        .into_iter()
        .find(|r| r.collection.as_deref() == Some(collection));
    let design = if review.is_none() {
        design_review::by_collection(run_dir, collection)
    } else {
        None
    };
    let answer = match (&review, &design) {
        (Some(r), _) => format!(
            "Answer with: cr reply --review {:03} --thread {thread} < reply.txt",
            r.number
        ),
        (None, Some(d)) => format!(
            "Answer with: design-review reply --page {} --thread {thread} < reply.txt",
            d.page
        ),
        (None, None) => format!("Answer in the reader: {base}/c/{collection}#{comment}"),
    };
    let his = &t.comments[at];
    let mut body = format!(
        "{}\n\n{}:{range} (version {})\n\n{}\n",
        his.body.trim_end(),
        doc.path,
        doc.version,
        source_block(&text, first, last)
    );
    if at > 0 {
        body.push_str("Thread so far:\n");
        for c in &t.comments[..at] {
            body.push_str(&format!(
                "\n{} at {}:\n{}\n",
                c.author,
                iso8601(c.created_at_ms / 1000),
                c.body.trim_end()
            ));
        }
        body.push('\n');
    }
    body.push_str(&answer);
    body.push('\n');
    Ok(format!(
        "From: Operator <operator@spira>\nTo: {MAILBOX}\nMessage-ID: {}\nDate: {}\nSubject: margin: {} — {}:{range}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\n{body}",
        message_id(comment),
        mail::rfc822::format_unix((his.created_at_ms / 1000) as i64),
        design.as_ref().map_or_else(|| header_text(&info.title), |d| format!("design {}", header_text(&d.page))),
        header_text(&doc.path),
    ))
}

/// Delivers `raw` to the Concierge unless its `Message-ID` is already in a mailbox.
pub fn deliver_once(d: &Delivery, raw: &str) -> Result<bool, String> {
    let mid = mail::message::header_ci_before_blank(raw, "message-id");
    if mail::sendmail::find_message_by_id(&d.mail_root, &mid).is_some() {
        return Ok(false);
    }
    mail::sendmail::deliver(
        &d.run_dir,
        &d.home,
        &d.mail_root,
        d.mute,
        raw,
        Some(MAILBOX),
    )
    .map(|_| true)
}

/// The one mail saying his pass on a design page is done, delivered once per collection and version.
pub fn publish_design(
    margin: &dyn Margin,
    d: &design_review::Record,
    delivery: &Delivery,
) -> Result<bool, String> {
    let info = margin.collection(&d.collection)?;
    let his = margin
        .threads(&d.collection)?
        .iter()
        .filter(|t| {
            t.document.version == d.version && t.comments.first().is_some_and(|c| !c.by_agent())
        })
        .count();
    let at = info.published_at_ms.unwrap_or(d.posted_at_ms);
    let page = header_text(&d.page);
    let raw = format!(
        "From: Operator <operator@spira>\nTo: {MAILBOX}\nMessage-ID: <margin-publish-{}-{}@spira>\nDate: {}\nSubject: margin: design {page} — his pass is published\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\nHis pass on {page} is done.\n\n{}\n\nHis threads on version {}: {his}\n",
        id_part(&d.collection),
        d.version,
        mail::rfc822::format_unix((at / 1000) as i64),
        d.url,
        d.version,
    );
    deliver_once(delivery, &raw)
}
