//! Shared by the fixtures: an evaluator for one expression at one point, a
//! windowed timing loop and the ledger writer. Cargo compiles this module once
//! per test binary that declares `mod common;`, and no binary uses all of it —
//! hence the allow.
//!
//! Only the pieces with a rule worth having one copy of live here. The
//! five-line helpers (`system`, `columns`, `variables`) stay duplicated in the
//! fixtures that use them; a shared module for those would be a dependency in
//! exchange for nothing.

#![allow(dead_code)]

use std::hint::black_box;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use faer::Mat;
use sojourn::{ConstraintSystem, InputVariable};
use tracing_subscriber::fmt::MakeWriter;

/// Everything a `tracing` subscriber wrote, readable after the run: what a
/// fixture uses to assert the *route* a call took — which stage answered,
/// whether COBYLA ran — rather than a clock. Scope the subscriber with
/// `tracing::subscriber::with_default` so nothing leaks into another test.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("no panic held the log")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for Captured {
    type Writer = Self;

    fn make_writer(&self) -> Self {
        self.clone()
    }
}

impl Captured {
    /// A subscriber at `trace` writing here, plain text.
    pub fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(self.clone())
            .finish()
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("no panic held the log").clone())
            .expect("the subscriber writes UTF-8")
    }
}

/// One expression at one point, as a one-column batch.
///
/// The crate deliberately has no such entry point: it rebuilds the variable
/// list, parses, binds, and allocates a row on every call, which is fine for
/// an assertion and wrong for anything in a loop. Fixtures that re-check a
/// delivered point against its sources want exactly this.
pub fn eval_one(
    source: &str,
    inputs: &[(&str, f64)],
) -> Result<f64, sojourn::diagnostics::EvaluationFailure> {
    let names: Vec<&str> = inputs.iter().map(|(name, _)| *name).collect();
    let compiled = sojourn::compile(source, &names)?;
    let sample = Mat::from_fn(inputs.len(), 1, |row, _| inputs[row].1);
    Ok(compiled.eval(sample.as_ref())?[0])
}

/// How long to run each case before reporting. Small enough in debug that a
/// normal test run barely notices.
pub const TARGET: Duration = if cfg!(debug_assertions) {
    Duration::from_millis(20)
} else {
    Duration::from_millis(400)
};

/// Best of this many, because scheduling noise only ever makes things slower.
pub const REPETITIONS: usize = 3;

/// The brute-force budget a pool test runs with.
///
/// The library's default is a billion proposals, sized for a release build:
/// a few seconds across a laptop's threads, spent only on what no seeder
/// could settle. Under a debug build the tape is some fifty times slower,
/// and a test whose probe is empty and whose constraints nothing can
/// conclude on — a computed subscript, a contradiction too thin to see — or
/// that leaves the seeders out would spend minutes brute-forcing. A million
/// keeps every rung of the ladder running at
/// debug speed and changes no verdict: a region a million proposals can land,
/// ten thousand would have landed one time in a hundred. Release runs the
/// real default, so `just brute` and the release benchmarks measure what a
/// caller gets.
pub const PROPOSAL_BUDGET: u64 = if cfg!(debug_assertions) {
    1_000_000
} else {
    sojourn::DEFAULT_PROPOSAL_BUDGET
};

/// Which instruction set the crate's tile kernels run on here, and how many
/// `f64` lanes that is: `("pulp::x86::v3::V3", 4)` on an AVX2 machine,
/// `("pulp::Scalar", 1)` without one. For the ledgers' host file.
///
/// Asked of `pulp` directly rather than of the crate: the crate dispatches
/// through the same `Arch::new()` and gets the same answer, and a test-only
/// probe is not something the crate should export.
fn simd_isa() -> (&'static str, usize) {
    struct Probe;
    impl pulp::WithSimd for Probe {
        type Output = (&'static str, usize);
        #[inline(always)]
        fn with_simd<S: pulp::Simd>(self, _: S) -> Self::Output {
            (std::any::type_name::<S>(), S::F64_LANES)
        }
    }
    pulp::Arch::new().dispatch(Probe)
}

/// Evaluations between clock reads. Amortises `Instant::now`, which is not free
/// and would otherwise dominate the cheapest case.
pub const CHUNK: usize = 64;

/// Runs `evaluate` until [`TARGET`] has elapsed, returning calls per
/// millisecond — best of [`REPETITIONS`] windows.
///
/// Calibrated by time rather than by a fixed iteration count, because the cases
/// span two orders of magnitude and any count that suits one would be absurd for
/// another. The closure's return value is accumulated and passed through
/// `black_box`, so a release build cannot delete the loop.
pub fn throughput(mut evaluate: impl FnMut(usize) -> f64) -> f64 {
    let mut best = 0.0f64;

    for _ in 0..REPETITIONS {
        let mut sink = 0.0f64;
        let mut count = 0usize;
        let start = Instant::now();

        loop {
            for _ in 0..CHUNK {
                sink += evaluate(count);
                count += 1;
            }
            if start.elapsed() >= TARGET {
                break;
            }
        }

        let elapsed = start.elapsed();
        // Without this the whole loop is dead code in a release build.
        black_box(sink);

        #[expect(
            clippy::cast_precision_loss,
            reason = "evaluation counts are far below the f64 integer limit"
        )]
        let rate = count as f64 / elapsed.as_secs_f64() / 1000.0;
        best = best.max(rate);
    }
    best
}

/// The line a report prints so that a pasted number cannot be misread.
pub fn profile_label() -> &'static str {
    if cfg!(debug_assertions) {
        "DEBUG — these numbers mean nothing, run `just bench`"
    } else {
        "release"
    }
}

/// Where the ledgers live, relative to `CARGO_MANIFEST_DIR` rather than the
/// working directory, so they land in the same place whether the runner was
/// invoked from the crate or the repo root.
pub const LEDGER_DIR: &str = "performance-records";

/// The machine a row was measured on. A number is only comparable within a
/// host, which is why every ledger carries this column.
///
/// `SOJOURN_HOST` wins when set, so a build agent or a borrowed machine can label
/// its rows deliberately; otherwise the OS's own name. Either way the label is
/// explained in `performance-records/hosts/README.md`, which is where the hardware
/// behind a name is recorded — the ledger only needs the key.
pub fn host() -> String {
    std::env::var("SOJOURN_HOST")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Upserts one row into `performance-records/<slug>.csv`, creating the file
/// with `header` if it is missing. Returns whether anything was written.
///
/// The row is preformatted by the caller so that its column widths sit next to
/// the header they have to match, in the file that owns both. This function
/// knows the upsert rule and nothing about columns.
///
/// Upsert rather than append because a tuning session is a dozen runs of one
/// version, and keeping all of them buries the history the file exists to show.
/// If the last row carries this version *and* this host it is replaced,
/// otherwise the row is appended. Bumping the version is what turns the
/// current row into a historical one.
///
/// **Release only.** The same tests run in debug at a fraction of the workload,
/// and under upsert that would not merely add a bad row — it would overwrite
/// the good one.
///
/// A missing ledger is created rather than treated as an error, so adding a
/// case needs no setup in this directory. Failures are printed, never panicked
/// on: a benchmark that cannot write its log has still produced its numbers,
/// and the numbers are on stdout.
pub fn record_row(slug: &str, header: &str, row: &str) -> bool {
    if cfg!(debug_assertions) {
        return false;
    }

    let version = env!("CARGO_PKG_VERSION");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(LEDGER_DIR)
        .join(format!("{slug}.csv"));

    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| header.to_owned());

    let mut lines: Vec<&str> = existing.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    // The previous run at this version *on this host*, if the last row is one.
    // Version alone is not the key: a row from another machine at the same
    // version is a different experiment, and replacing it is losing data. It
    // happened — the laptop's first run overwrote BATOU's `2.1.0-native` rows.
    let this_host = host();
    if lines.last().is_some_and(|line| {
        let mut fields = line.split(';');
        fields
            .next()
            .is_some_and(|recorded| recorded.trim() == version)
            && fields
                .nth(1)
                .is_some_and(|recorded| recorded.trim() == this_host)
    }) {
        lines.pop();
    }

    let mut updated: String = lines.iter().map(|line| format!("{line}\n")).collect();
    updated.push_str(row);
    updated.push('\n');

    match std::fs::write(&path, updated) {
        Ok(()) => true,
        Err(e) => {
            println!("could not write {}: {e}", path.display());
            false
        }
    }
}

/// Writes a dozen-line description of this machine to
/// `performance-records/hosts/<host>.txt`, so the `host` column in a ledger
/// can be looked up. Returns whether the file changed.
///
/// Coarse on purpose: which machine, and roughly what class of machine. A
/// reader of a ledger does not need the PCI registers. Everything comes from
/// `sysinfo`, so it is the same dozen lines on Windows, Linux and macOS with no
/// elevation and nothing installed.
///
/// Written only when the content differs from what is on disk, and carries no
/// timestamp, so a debug run in CI or a dozen tuning runs leave no churn — a
/// diff here means the hardware or the toolchain moved.
pub fn describe_host() -> bool {
    use sysinfo::{CpuRefreshKind, System};

    let mut system = System::new();
    system.refresh_cpu_list(CpuRefreshKind::everything());
    system.refresh_memory();

    let cpu = system.cpus().first();
    let threads = system.cpus().len();
    let cores = System::physical_core_count().unwrap_or(threads);
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(
            || "unknown".to_owned(),
            |v| v.trim().trim_start_matches("rustc ").to_owned(),
        );

    let (isa, lanes) = simd_isa();
    #[cfg(feature = "gpu")]
    let gpu = sojourn::gpu::adapter_name().unwrap_or_else(|| "none".to_owned());
    #[cfg(not(feature = "gpu"))]
    let gpu = "not built".to_owned();
    let description = format!(
        "host:     {}\n\
         machine:  {}\n\
         os:       {} ({})\n\
         cpu:      {}\n\
         cores:    {cores} cores / {threads} threads, {} MHz\n\
         ram:      {} GB\n\
         simd:     {isa}, {lanes} f64 lanes\n\
         gpu:      {gpu}\n\
         rustc:    {rustc}\n",
        host(),
        System::host_name().unwrap_or_else(|| "unknown".to_owned()),
        System::long_os_version().unwrap_or_else(|| "unknown".to_owned()),
        System::cpu_arch(),
        cpu.map_or("unknown", |cpu| cpu.brand().trim()),
        cpu.map_or(0, sysinfo::Cpu::frequency),
        system.total_memory().div_ceil(1 << 30),
    );

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(LEDGER_DIR)
        .join("hosts");
    let path = directory.join(format!("{}.txt", host()));

    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == description) {
        return false;
    }
    if let Err(e) =
        std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&path, description))
    {
        println!("could not write {}: {e}", path.display());
        return false;
    }
    println!("described this host in {}", path.display());
    true
}

/// An RFC 3339 timestamp, to the second, in UTC.
///
/// Hand-rolled because the alternative is a `chrono` or `time` dependency in a
/// crate that has neither, for one line in a benchmark. Civil-from-days is
/// Howard Hinnant's algorithm, which is the standard one and gets the leap year
/// rules exactly right.
pub fn timestamp_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3600, (rest % 3600) / 60, rest % 60);

    // Shift the epoch to 0000-03-01 so leap days fall at the end of the cycle.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;

    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = era * 400 + year_of_era + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The stepped cantilever (Vanderplaats 1984; OASIS `Samples/PowerShell/Stepped
/// Beam 100` for the constants and box), `n` segments: `b1..bn in [1, 5]`,
/// `h1..hn in [5, 100]`; per segment a bending-stress limit and the aspect
/// ratio `h_i <= 20 b_i`; one tip-deflection limit coupling every segment.
/// Artemis's `e06`; the all-max corner is feasible. Shared because it is the
/// scale the search is expected to reach and more than one fixture measures
/// against it.
pub fn stepped_beam(n: usize) -> anyhow::Result<ConstraintSystem> {
    const P: f64 = 50000.0;
    const E: f64 = 2.0e7;
    const L: f64 = 500.0;
    const SIG: f64 = 14000.0;
    const YMX: f64 = 2.54;
    const ASPECT: f64 = 20.0;
    #[expect(clippy::cast_precision_loss, reason = "a segment count is small")]
    let l = L / n as f64;
    let mut vars: Vec<InputVariable> = (1..=n)
        .map(|i| InputVariable::new_continuous(format!("b{i}"), 1.0, 5.0))
        .collect();
    vars.extend((1..=n).map(|i| InputVariable::new_continuous(format!("h{i}"), 5.0, 100.0)));
    let mut constraints = Vec::with_capacity(2 * n + 1);
    for i in 1..=n {
        #[expect(clippy::cast_precision_loss, reason = "a segment index is small")]
        let m = P * (L + l - i as f64 * l);
        constraints.push(format!(
            "(0.5 * {m} * h{i}) / (b{i} * (h{i}^3) / 12) / {SIG} - 1 < 0"
        ));
    }
    for i in 1..=n {
        constraints.push(format!("h{i} - {ASPECT} * b{i} < 0"));
    }
    let inertia = format!("(var[i] * (var[i + {n}]^3) / 12)");
    let local = format!(
        "sum(1, {n}, i -> 0.5 * {P} * {l} * {l} * ({L} - i * {l} + 2 * {l} / 3) / ({E} * {inertia}))"
    );
    let slope = format!(
        "sum(1, {n}, i -> {P} * {l} * ({L} + 0.5 * {l} - i * {l}) / ({E} * {inertia}) * {l} * ({n} - i))"
    );
    constraints.push(format!("({local} + {slope}) / {YMX} - 1 < 0"));
    Ok(ConstraintSystem::new(vars, constraints)?)
}
