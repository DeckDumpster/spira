//! The archivist's own prompt templating: substituting `{{PLACEHOLDER}}` tokens into
//! `chamber/archivist.md`, and splitting the rendered prompt into a system file and a
//! task file on `<!-- task -->` — ported from lib.sh's `system_prompt_split` directly
//! (not through the seam) because it is pure string manipulation with no shared state
//! any other script reads or writes; lib.sh's own copy is untouched and still serves
//! every other caller.

/// `${x//\{\{KEY\}\}/value}` for each `(KEY, value)` pair, applied left to right exactly
/// as the bash did.
pub fn substitute(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Split a rendered prompt on `<!-- task -->` into `(system_file_contents,
/// task_file_contents)`. Archivist always renders with `FAYTH_SYSTEM_PROMPT=replace`, so
/// its system flag is always `--system-prompt-file` — unlike a `.fayth` persona, which can
/// ask for append instead; this crate has no caller that does, so that branch is not
/// reproduced here.
pub fn system_prompt_split(statutes: &str, prompt: &str) -> (String, String) {
    const MARK: &str = "<!-- task -->";
    let (sys, task) = match prompt.find(MARK) {
        Some(idx) => {
            let sys = &prompt[..idx];
            let task = prompt[idx + MARK.len()..].strip_prefix('\n').unwrap_or(&prompt[idx + MARK.len()..]);
            (sys, task)
        }
        None => ("", prompt),
    };
    (format!("# Memories in force\n\n{statutes}\n\n---\n\n{sys}"), task.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_replaces_every_occurrence_of_each_placeholder() {
        let out = substitute("hi {{NAME}}, {{NAME}} again, {{OTHER}}", &[("NAME", "sid-1"), ("OTHER", "x")]);
        assert_eq!(out, "hi sid-1, sid-1 again, x");
    }

    #[test]
    fn substitute_leaves_an_unmatched_placeholder_untouched() {
        let out = substitute("{{KNOWN}} {{UNKNOWN}}", &[("KNOWN", "v")]);
        assert_eq!(out, "v {{UNKNOWN}}");
    }

    #[test]
    fn system_prompt_split_separates_on_the_task_marker() {
        let (sys, task) = system_prompt_split("STATUTES", "system part\n<!-- task -->\ntask part");
        assert_eq!(sys, "# Memories in force\n\nSTATUTES\n\n---\n\nsystem part\n");
        assert_eq!(task, "task part");
    }

    #[test]
    fn a_prompt_with_no_marker_is_entirely_the_task() {
        let (sys, task) = system_prompt_split("", "just a task, no split");
        assert_eq!(sys, "# Memories in force\n\n\n\n---\n\n");
        assert_eq!(task, "just a task, no split");
    }
}
