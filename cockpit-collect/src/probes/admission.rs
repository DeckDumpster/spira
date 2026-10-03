//! `admission_keys` — each admission pool's held/size/waiting/oldest-wait, read through the
//! same occupancy `spira-admit status` prints. An unreadable pool is `?`, never zero.

use super::{push, Kv};
use crate::io;
use spira_config::admission::{self, Pool, RealProcs};

pub fn admission_keys() -> Kv {
    admission_keys_in(&io::run_dir(), admission::size_configured)
}

fn admission_keys_in(run: &std::path::Path, size_of: fn(Pool) -> u64) -> Kv {
    let now = admission::now_epoch();
    let mut out = Kv::new();
    for pool in Pool::ALL {
        let p = pool.name().to_uppercase();
        let size = size_of(pool);
        let occ = match pool {
            Pool::Gate => Some(admission::gate_occupancy(run, size)),
            _ => admission::read_occupancy(run, pool, size, &RealProcs),
        };
        let (held, waiting, oldest, blocked) = match &occ {
            Some(o) => (
                o.used().to_string(),
                o.waiting.to_string(),
                o.waiters.first().map_or("0".to_string(), |w| now.saturating_sub(w.since).to_string()),
                if o.head_blocked().is_some() { "1" } else { "0" }.to_string(),
            ),
            None => ("?".into(), "?".into(), "?".into(), "?".into()),
        };
        push(&mut out, &format!("SP_ADM_{p}_SIZE"), size.to_string());
        push(&mut out, &format!("SP_ADM_{p}_HELD"), held);
        push(&mut out, &format!("SP_ADM_{p}_WAITING"), waiting);
        push(&mut out, &format!("SP_ADM_{p}_OLDEST"), oldest);
        push(&mut out, &format!("SP_ADM_{p}_BLOCKED"), blocked);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(kv: &'a Kv, k: &str) -> &'a str {
        &kv.iter().find(|(key, _)| key == k).unwrap().1
    }

    fn size(p: Pool) -> u64 {
        if p == Pool::Gate { 3 } else { 2 }
    }

    #[test]
    fn an_empty_pool_reads_zero_and_the_configured_size() {
        let run = testkit::TempDir::new("cc-adm-empty");
        let kv = admission_keys_in(run.path(), size);
        assert_eq!(get(&kv, "SP_ADM_GATE_SIZE"), "3");
        assert_eq!(get(&kv, "SP_ADM_COMPILE_HELD"), "0");
        assert_eq!(get(&kv, "SP_ADM_TEST_OLDEST"), "0");
    }

    #[test]
    fn a_pool_whose_mutex_cannot_be_taken_reads_question_marks() {
        let run = testkit::TempDir::new("cc-adm-unreadable");
        std::fs::write(Pool::Compile.dir(run.path()), "not a directory").unwrap();
        let kv = admission_keys_in(run.path(), size);
        assert_eq!(get(&kv, "SP_ADM_COMPILE_HELD"), "?");
        assert_eq!(get(&kv, "SP_ADM_COMPILE_WAITING"), "?");
        assert_eq!(get(&kv, "SP_ADM_TEST_HELD"), "0");
    }
}
