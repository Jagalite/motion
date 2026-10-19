//! Closed observation companion for the pinned native runtime. No generic
//! command dispatch, external processors, shell or legacy worker is exposed.
use catabolic_runtime::{helper, observation};
use catabolic_store::{Error, Result};
use serde_json::Value;
use std::path::Path;

fn run(args: &[String]) -> Result<Value> {
    let [db_flag, db, profile_flag, profile, command, payload] = args else {
        return Err(Error("expected exact native observation invocation".into()));
    };
    if db_flag != "--db" || profile_flag != "--profile" || payload.len() > 1024 * 1024 {
        return Err(Error("invalid native observation invocation".into()));
    }
    let execute = match command.as_str() {
        observation::CLOSED_WORKER_COMMAND => observation::worker_v2,
        observation::SHARED_WORKER_COMMAND => observation::shared_worker_v1,
        _ => return Err(Error("unsupported observation protocol".into())),
    };
    let request = catabolic_core::decode_json(payload)?;
    catabolic_store::filesystem_mutation::admit_failpoints(&request)?;
    helper::register(&std::env::current_exe()?, None)?;
    execute(Path::new(db), profile, &request)
}
fn main() {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("motion-catalog-observer: {error}");
            std::process::exit(2);
        }
    }
}
