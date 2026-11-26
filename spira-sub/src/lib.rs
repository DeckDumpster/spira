//! One subscriber's single broker connection. [`core`] decides what is delivered and is pure;
//! [`run`] drives it over MQTT and the control socket, and [`client`] is the command side.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

pub mod cli;
pub mod client;
pub mod core;
pub mod protocol;
mod render;
pub mod run;
