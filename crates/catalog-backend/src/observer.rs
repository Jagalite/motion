//! Packaging-owned closed observation protocol. The shipped server and operator
//! executables invoke this dispatcher before their ordinary command parsers or
//! runtimes, so the native helper executes only the two audited closed branches.
//! No caller-supplied executable path or digest can mint a packaging grant.
use catabolic_runtime::{helper, observation};
use catabolic_store::{Error, Result};
use serde_json::Value;
use std::{io::Read, path::Path};

pub fn run_closed(args: &[String]) -> Result<Value> {
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

/// Call before application initialization. `Some(code)` means the invocation is
/// exclusively a companion invocation and the executable must exit with code.
pub fn dispatch_builtin_invocation() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_none_or(|s| s != "--db") {
        return None;
    }
    Some(match run_closed(&args) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            eprintln!("motion-catalog-observer: {error}");
            2
        }
    })
}

/// Trusted package boundary for the executable containing this dispatcher.
/// Only call from the shipped Motion server/operator which unconditionally
/// dispatches companion invocations before ordinary application initialization.
/// A test harness or arbitrary third-party host is not a verified package.
/// The file is bounded and regular before reading, preventing FIFO/path hangs.
pub fn packaged_current_executable() -> anyhow::Result<helper::TrustedClosedObservationArtifact> {
    let path = std::env::current_exe()?.canonicalize()?;
    let mut file = std::fs::File::open(&path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 128 * 1024 * 1024,
        "packaged observer must be a bounded regular executable"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == metadata.len(),
        "packaged observer changed while reading"
    );
    Ok(
        helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &path,
            &catabolic_core::sha256(bytes),
            helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_every_nonclosed_protocol_before_opening_catalog() {
        for command in [
            "scan",
            "observation-worker",
            "native-observation-worker-v1",
            "shell",
        ] {
            let args = [
                "--db",
                "/must-not-open",
                "--profile",
                "default",
                command,
                "{}",
            ]
            .map(str::to_owned);
            assert!(run_closed(&args).is_err());
        }
    }
    #[test]
    fn rejects_invalid_shape_and_oversized_payload_before_native_dispatch() {
        assert!(run_closed(&[]).is_err());
        let args = vec![
            "--db".into(),
            "/must-not-open".into(),
            "--profile".into(),
            "default".into(),
            observation::CLOSED_WORKER_COMMAND.into(),
            "x".repeat(1024 * 1024 + 1),
        ];
        assert!(run_closed(&args).is_err());
    }
}
