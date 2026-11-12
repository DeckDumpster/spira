/// A line for the caller's stderr: the binary prints it, an in-process caller reads it from
/// [`client::Reply::err`]. Kept per thread, because one verb runs on one thread.
macro_rules! warn {
    ($($t:tt)*) => {
        $crate::diag::push(format!($($t)*))
    };
}

pub mod client;
pub mod cmds;
pub mod diag;
pub mod fork_scan;
pub mod ports;
pub mod real;
pub mod spend;
pub mod telemetry;
pub mod time;
pub mod zipread;

#[cfg(test)]
mod tests;
