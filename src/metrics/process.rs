//! Current-process observations only; none of these values decide application state.
//!
//! The caller runs this bounded native/filesystem work on a blocking thread. No
//! command lines, descriptor targets, environment, or other process data escape.

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub pid: String,
    pub cpu_seconds_total: String,
    pub resident_bytes: String,
    pub virtual_bytes: Option<String>,
    pub open_files: Option<String>,
    pub threads: Option<String>,
    pub cpu_count: String,
}

/// Mandatory observations were unavailable. Deliberately carries no OS error
/// text or paths, so it is safe to use in stable diagnostic logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotError;

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("process_metrics_unavailable")
    }
}

impl std::error::Error for SnapshotError {}

/// Returns an observation, never fabricated zeros for failed mandatory fields.
/// Optional fields serialize as explicit nulls when their observations fail.
pub fn snapshot() -> Result<Snapshot, SnapshotError> {
    platform_snapshot()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cpu_seconds() -> Result<String, SnapshotError> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `usage` points to writable, properly aligned storage. On success,
    // getrusage initializes it. RUSAGE_SELF sums every thread, excluding children.
    // https://www.man7.org/linux/man-pages/man2/getrusage.2.html
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(SnapshotError);
    }
    // SAFETY: the call above succeeded and initialized the structure.
    let usage = unsafe { usage.assume_init() };
    format_cpu_seconds(
        i128::from(usage.ru_utime.tv_sec),
        i128::from(usage.ru_utime.tv_usec),
        i128::from(usage.ru_stime.tv_sec),
        i128::from(usage.ru_stime.tv_usec),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn format_cpu_seconds(
    user_seconds: i128,
    user_micros: i128,
    system_seconds: i128,
    system_micros: i128,
) -> Result<String, SnapshotError> {
    fn micros(seconds: i128, fraction: i128) -> Result<u128, SnapshotError> {
        if !(0..1_000_000).contains(&fraction) {
            return Err(SnapshotError);
        }
        u128::try_from(seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000))
            .and_then(|seconds| seconds.checked_add(fraction as u128))
            .ok_or(SnapshotError)
    }
    let total = micros(user_seconds, user_micros)?
        .checked_add(micros(system_seconds, system_micros)?)
        .ok_or(SnapshotError)?;
    // Integer arithmetic retains exact cumulative values beyond JSON/f64's 2^53.
    Ok(format!("{}.{:06}", total / 1_000_000, total % 1_000_000))
}

#[cfg(target_os = "macos")]
fn platform_snapshot() -> Result<Snapshot, SnapshotError> {
    let pid = std::process::id();
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: proc_taskinfo matches PROC_PIDTASKINFO's ABI and the buffer size.
    // Only assume initialization after the kernel reports the full structure.
    // Current resident/virtual sizes are bytes, not getrusage's peak ru_maxrss.
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return Err(SnapshotError);
    }
    // SAFETY: a complete proc_taskinfo was returned above.
    let info = unsafe { info.assume_init() };
    Ok(Snapshot {
        pid: pid.to_string(),
        cpu_seconds_total: cpu_seconds()?,
        resident_bytes: info.pti_resident_size.to_string(),
        virtual_bytes: Some(info.pti_virtual_size.to_string()),
        open_files: macos_open_files(pid).map(|value| value.to_string()),
        threads: u32::try_from(info.pti_threadnum)
            .ok()
            .filter(|count| *count > 0)
            .map(|value| value.to_string()),
        cpu_count: std::thread::available_parallelism()
            .map_err(|_| SnapshotError)?
            .get()
            .to_string(),
    })
}

#[cfg(target_os = "macos")]
fn macos_open_files(pid: u32) -> Option<usize> {
    const MAX_DESCRIPTORS: usize = 65_536;
    let item_size = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer with zero size asks only for the required size.
    // This is a capacity estimate, not the number of open descriptors.
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
    let estimated_bytes = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if estimated_bytes <= 0 {
        return None;
    }
    let mut capacity = (estimated_bytes as usize)
        .div_ceil(item_size)
        .min(MAX_DESCRIPTORS);
    // The descriptor table can grow between calls. Retry a full buffer, but
    // bound both work and memory; never publish a silently truncated count.
    for _ in 0..3 {
        let mut entries = Vec::<std::mem::MaybeUninit<libc::proc_fdinfo>>::new();
        entries.try_reserve_exact(capacity).ok()?;
        entries.resize_with(capacity, std::mem::MaybeUninit::uninit);
        let buffer_bytes = (capacity * item_size) as libc::c_int;
        // SAFETY: entries provides the exact writable length and alignment.
        // Only the returned byte count is consumed; no uninitialized items read.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDLISTFDS,
                0,
                entries.as_mut_ptr().cast(),
                buffer_bytes,
            )
        };
        if written < 0 || written > buffer_bytes || !(written as usize).is_multiple_of(item_size) {
            return None;
        }
        // libproc reports failures as zero. Treat that ambiguity as unavailable
        // rather than inventing an empty descriptor table.
        if written == 0 {
            return None;
        }
        if written < buffer_bytes {
            return Some(written as usize / item_size);
        }
        if capacity == MAX_DESCRIPTORS {
            return None;
        }
        capacity = (capacity * 2).min(MAX_DESCRIPTORS);
    }
    None
}

#[cfg(target_os = "linux")]
fn platform_snapshot() -> Result<Snapshot, SnapshotError> {
    use std::io::Read;

    const MAX_STATUS_BYTES: u64 = 65_536;
    let mut status = String::new();
    std::fs::File::open("/proc/self/status")
        .map_err(|_| SnapshotError)?
        .take(MAX_STATUS_BYTES + 1)
        .read_to_string(&mut status)
        .map_err(|_| SnapshotError)?;
    if status.len() as u64 > MAX_STATUS_BYTES {
        return Err(SnapshotError);
    }
    linux_status_snapshot(&status, cpu_seconds()?, linux_open_files())
}

#[cfg(any(target_os = "linux", test))]
fn linux_status_snapshot(
    status: &str,
    cpu_seconds_total: String,
    open_files: Option<u64>,
) -> Result<Snapshot, SnapshotError> {
    fn value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
        status.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name == key).then_some(value.trim())
        })
    }
    fn bytes(status: &str, key: &str) -> Option<u64> {
        let mut fields = value(status, key)?.split_whitespace();
        let number = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" || fields.next().is_some() {
            return None;
        }
        number.checked_mul(1024)
    }
    // /proc/self/status reports current VmRSS, separately from peak VmHWM.
    // Linux accounts RSS asynchronously, so this is its inexpensive current
    // estimate (like statm), not a page-table walk or a high-water mark.
    // https://docs.kernel.org/filesystems/proc.html
    let resident_bytes = bytes(status, "VmRSS").ok_or(SnapshotError)?;
    // Count the actual allowed logical CPUs, not CFS quota equivalents or all
    // host CPUs. The mask is observed for the process's main thread.
    let allowed = value(status, "Cpus_allowed").ok_or(SnapshotError)?;
    let mut cpu_count = 0u64;
    for digit in allowed.chars().filter(|digit| *digit != ',') {
        cpu_count += u64::from(digit.to_digit(16).ok_or(SnapshotError)?.count_ones());
    }
    if cpu_count == 0 {
        return Err(SnapshotError);
    }
    Ok(Snapshot {
        pid: std::process::id().to_string(),
        cpu_seconds_total,
        resident_bytes: resident_bytes.to_string(),
        virtual_bytes: bytes(status, "VmSize").map(|value| value.to_string()),
        open_files: open_files.map(|value| value.to_string()),
        threads: value(status, "Threads")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|count| *count > 0)
            .map(|value| value.to_string()),
        cpu_count: cpu_count.to_string(),
    })
}

#[cfg(target_os = "linux")]
fn linux_open_files() -> Option<u64> {
    const MAX_DESCRIPTORS: u64 = 65_536;
    let mut count = 0u64;
    for entry in std::fs::read_dir("/proc/self/fd").ok()? {
        entry.ok()?;
        count += 1;
        if count > MAX_DESCRIPTORS {
            return None;
        }
    }
    // read_dir keeps its own directory descriptor open for the entire walk;
    // remove that observation-only descriptor from the result.
    count.checked_sub(1)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_snapshot() -> Result<Snapshot, SnapshotError> {
    Err(SnapshotError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_total_adds_user_and_system_without_float_rounding() {
        assert_eq!(
            format_cpu_seconds(3, 900_000, 2, 250_001).unwrap(),
            "6.150001"
        );
        assert_eq!(
            format_cpu_seconds(9_007_199_254_740_993, 1, 0, 0).unwrap(),
            "9007199254740993.000001"
        );
        for arguments in [(-1, 0, 0, 0), (0, -1, 0, 0), (0, 1_000_000, 0, 0)] {
            assert!(
                format_cpu_seconds(arguments.0, arguments.1, arguments.2, arguments.3).is_err()
            );
        }
    }

    #[test]
    fn linux_observations_use_current_rss_and_allowed_logical_cpus() {
        let status = "Name:\tprivate executable\nVmHWM:\t65536 kB\nVmRSS:\t128 kB\nVmSize:\t8192 kB\nThreads:\t7\nCpus_allowed:\t00000001,00000005\n";
        let sample = linux_status_snapshot(status, "1.500000".into(), Some(23)).unwrap();
        assert_eq!(sample.resident_bytes, "131072");
        assert_eq!(sample.virtual_bytes.as_deref(), Some("8388608"));
        assert_eq!(sample.threads.as_deref(), Some("7"));
        assert_eq!(sample.cpu_count, "3");
        assert_eq!(sample.open_files.as_deref(), Some("23"));
        let wire = serde_json::to_string(&sample).unwrap();
        assert!(!wire.contains("private executable"));
        assert!(!wire.contains("65536"));
    }

    #[test]
    fn unavailable_optional_fields_serialize_as_null_without_fabricated_values() {
        let sample = linux_status_snapshot(
            "VmRSS: 1 kB\nCpus_allowed: f\nVmSize: invalid\nThreads: invalid\n",
            "0.000000".into(),
            None,
        )
        .unwrap();
        let wire = serde_json::to_value(sample).unwrap();
        assert_eq!(wire["virtual_bytes"], serde_json::Value::Null);
        assert_eq!(wire["open_files"], serde_json::Value::Null);
        assert_eq!(wire["threads"], serde_json::Value::Null);
        assert_eq!(wire["resident_bytes"], "1024");
        assert_eq!(wire.as_object().unwrap().len(), 7);
    }

    #[test]
    fn unavailable_or_invalid_mandatory_observations_fail_closed() {
        for status in [
            "VmHWM: 1 kB\nCpus_allowed: 1\n",
            "VmRSS: 18446744073709551615 kB\nCpus_allowed: 1\n",
            "VmRSS: 1 MB\nCpus_allowed: 1\n",
            "VmRSS: 1 kB\nCpus_allowed: 0\n",
            "VmRSS: 1 kB\nCpus_allowed: bad-mask\n",
            "VmRSS: 1 kB\n",
        ] {
            let failure = linux_status_snapshot(status, "0.000000".into(), None).unwrap_err();
            assert_eq!(failure.to_string(), "process_metrics_unavailable");
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn native_snapshot_is_numeric_and_process_cpu_never_decreases() {
        let first = snapshot().unwrap();
        assert_eq!(first.pid, std::process::id().to_string());
        assert!(first.resident_bytes.parse::<u64>().unwrap() > 0);
        assert!(first.cpu_count.parse::<u64>().unwrap() > 0);
        for optional in [&first.virtual_bytes, &first.open_files, &first.threads] {
            if let Some(value) = optional {
                value.parse::<u64>().unwrap();
            }
        }
        let mut result = 1u64;
        for number in 0..100_000u64 {
            result = std::hint::black_box(result.wrapping_mul(31).wrapping_add(number));
        }
        std::hint::black_box(result);
        let second = snapshot().unwrap();
        let first_cpu = first.cpu_seconds_total.parse::<f64>().unwrap();
        let second_cpu = second.cpu_seconds_total.parse::<f64>().unwrap();
        assert!(first_cpu.is_finite() && first_cpu >= 0.0);
        assert!(second_cpu.is_finite() && second_cpu >= first_cpu);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn native_open_file_observation_includes_owned_descriptors() {
        // A lower bound remains valid while other tests concurrently open and
        // close files; exact before/after deltas would make this test flaky.
        let files: Vec<_> = (0..16)
            .map(|_| std::fs::File::open("/dev/null").unwrap())
            .collect();
        let sample = snapshot().unwrap();
        let count = sample.open_files.unwrap().parse::<usize>().unwrap();
        assert!(count >= files.len());
        std::hint::black_box(files);
    }
}
