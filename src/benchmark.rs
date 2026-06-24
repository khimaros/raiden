// the fsync-bound sysbench fileio benchmark, run on the mounted root fs (the
// array). every write is fsync'd (--file-fsync-all=on), so the number is durable
// write latency -- the metric that matters for crash-consistent raid, not
// page-cache throughput. lower is better. ported from the harness's old
// guest/benchmark.sh so the workload lives in one place and runs anywhere raiden
// is installed.

use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::Benchmark as Cfg;
use crate::step::{Phase, Step};

// scratch dir on the root fs. NOT /tmp: systemd mounts /tmp as tmpfs (ram), which
// would benchmark ram instead of the raid stack and cannot hold the working set.
// /var/tmp is on the root filesystem per the fhs.
const WORKDIR: &str = "/var/tmp/raiden-benchmark";

/// the resolved benchmark parameters (config defaults overlaid by flags).
pub struct Bench {
    pub size: String,
    pub passes: u32,
    pub rndwr_events: u64,
    pub seqwr_events: u64,
}

/// one mode's aggregate across passes: average total time and average p95.
struct ModeResult {
    mode: &'static str,
    total_s: f64,
    p95_ms: f64,
}

impl Bench {
    pub fn from_cfg(c: &Cfg) -> Self {
        Self {
            size: c.size.clone(),
            passes: c.passes,
            rndwr_events: c.rndwr_events,
            seqwr_events: c.seqwr_events,
        }
    }

    // random writes churn more per event than sequential, so they need fewer.
    fn modes(&self) -> [(&'static str, u64); 2] {
        [("rndwr", self.rndwr_events), ("seqwr", self.seqwr_events)]
    }

    fn prepare_cmd(&self) -> String {
        format!(
            "cd {WORKDIR} && sysbench fileio prepare --file-total-size={}",
            self.size
        )
    }

    fn run_cmd(&self, mode: &str, events: u64) -> String {
        format!(
            "cd {WORKDIR} && sysbench fileio run --file-total-size={} \
             --file-test-mode={mode} --file-fsync-all=on --time=0 --events={events}",
            self.size
        )
    }
}

/// the benchmark as an ordered plan, for `--dry-run`. mirrors `run` step for step
/// so the dry-run shows the exact sysbench invocations that would execute.
pub fn plan(b: &Bench) -> Vec<Phase> {
    let mut s = vec![
        Step::run(
            "ensure sysbench is installed",
            &["apt-get", "install", "-y", "sysbench"],
        )
        .best_effort(),
        Step::run_owned(
            "create the scratch dir on the root fs",
            vec!["mkdir".to_string(), "-p".to_string(), WORKDIR.to_string()],
        ),
        Step::sh("prepare the fileio working set", b.prepare_cmd()),
    ];
    for (mode, events) in b.modes() {
        for i in 1..=b.passes {
            s.push(Step::sh(
                format!("{mode} pass {i}/{}", b.passes),
                b.run_cmd(mode, events),
            ));
        }
    }
    s.push(Step::run_owned(
        "remove the scratch dir",
        vec!["rm".to_string(), "-rf".to_string(), WORKDIR.to_string()],
    ));
    vec![Phase::new("benchmark", s)]
}

/// run the benchmark for real: prepare, run every pass capturing its output,
/// parse the durable-write metrics, and print a per-mode summary (or json).
pub fn run(b: &Bench, format: &str) -> Result<()> {
    let json = match format {
        "text" | "json" => format == "json",
        other => bail!("--format must be \"text\" or \"json\", got {other:?}"),
    };
    // best-effort: sysbench is usually preinstalled (the harness adds it as a
    // package); install it if missing, but let a later failure report the cause.
    let _ = run_argv(&["apt-get", "install", "-y", "sysbench"]);
    run_argv(&["mkdir", "-p", WORKDIR])?;
    shell(&b.prepare_cmd()).context("preparing the fileio working set")?;

    let mut results = Vec::new();
    for (mode, events) in b.modes() {
        let mut totals = Vec::new();
        let mut p95s = Vec::new();
        for i in 1..=b.passes {
            let out = shell(&b.run_cmd(mode, events))
                .with_context(|| format!("running sysbench {mode} pass {i}"))?;
            let (total, p95) = parse_pass(&out)
                .with_context(|| format!("parsing sysbench {mode} pass {i} output"))?;
            totals.push(total);
            p95s.push(p95);
            if !json {
                println!(
                    "  {mode} pass {i}/{}: total {total:.1}s  p95 {p95:.1}ms",
                    b.passes
                );
            }
        }
        results.push(ModeResult {
            mode,
            total_s: mean(&totals),
            p95_ms: mean(&p95s),
        });
    }
    let _ = run_argv(&["rm", "-rf", WORKDIR]);

    if json {
        print_json(b, &results);
    } else {
        print_table(b, &results);
    }
    Ok(())
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// pull the durable-write metrics out of one sysbench fileio run: total time
/// (seconds) and the 95th-percentile latency (ms). returns None if either line is
/// missing, which the caller turns into a clear parse error.
fn parse_pass(out: &str) -> Option<(f64, f64)> {
    let mut total = None;
    let mut p95 = None;
    for line in out.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("total time:") {
            total = rest.trim().trim_end_matches('s').parse().ok();
        } else if l.starts_with("95th percentile:") {
            p95 = l.rsplit(':').next()?.trim().parse().ok();
        }
    }
    Some((total?, p95?))
}

fn print_table(b: &Bench, results: &[ModeResult]) {
    println!(
        "\nbenchmark (sysbench fileio, fsync-all, {}, {} passes)",
        b.size, b.passes
    );
    println!("  {:<6} {:>12} {:>10}", "mode", "avg total", "avg p95");
    for r in results {
        println!("  {:<6} {:>11.1}s {:>8.1}ms", r.mode, r.total_s, r.p95_ms);
    }
}

/// machine-readable summary for the test harness. hand-rolled to avoid a json
/// dependency for this tiny payload.
fn print_json(b: &Bench, results: &[ModeResult]) {
    let body: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                "\"{}\":{{\"total_s\":{:.3},\"p95_ms\":{:.3}}}",
                r.mode, r.total_s, r.p95_ms
            )
        })
        .collect();
    println!(
        "{{\"size\":\"{}\",\"passes\":{},{}}}",
        b.size,
        b.passes,
        body.join(",")
    );
}

fn shell(cmd: &str) -> Result<String> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .with_context(|| format!("running: {cmd}"))?;
    if !out.status.success() {
        bail!(
            "command failed: {cmd}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_argv(argv: &[&str]) -> Result<()> {
    let status = Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("running {}", argv.join(" ")))?;
    if !status.success() {
        bail!("command failed: {}", argv.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
File operations:
    reads/s:                      0.00
    writes/s:                     405.12
Throughput:
    written, MiB/s:               6.33
Latency (ms):
         min:                                    0.12
         avg:                                    2.46
         max:                                   41.10
         95th percentile:                       37.56
         sum:                                12345.67
General statistics:
    total time:                          142.0123s
    total number of events:              5000
";

    #[test]
    fn parses_total_time_and_p95() {
        let (total, p95) = parse_pass(SAMPLE).unwrap();
        assert!((total - 142.0123).abs() < 1e-6, "total={total}");
        assert!((p95 - 37.56).abs() < 1e-6, "p95={p95}");
    }

    #[test]
    fn missing_metrics_fail_to_parse() {
        assert!(parse_pass("no useful lines here").is_none());
    }

    #[test]
    fn plan_emits_a_pass_per_mode_per_count() {
        let b = Bench {
            size: "1G".into(),
            passes: 2,
            rndwr_events: 100,
            seqwr_events: 200,
        };
        let phases = plan(&b);
        let notes: Vec<&str> = phases[0].steps.iter().map(|s| s.note.as_str()).collect();
        // prepare + 2 rndwr + 2 seqwr passes are all present.
        assert!(notes.contains(&"rndwr pass 1/2") && notes.contains(&"rndwr pass 2/2"));
        assert!(notes.contains(&"seqwr pass 1/2") && notes.contains(&"seqwr pass 2/2"));
    }
}
