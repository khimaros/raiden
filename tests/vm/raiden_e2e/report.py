"""result model and markdown report generation."""

from __future__ import annotations

from dataclasses import dataclass, field

# grading vocabulary, mirroring tests/analysis in raid-explorations.
PASS = "pass"
WARN = "warn"
FAIL = "fail"
INFO = "info"

MARK = {PASS: "PASS", WARN: "WARN", FAIL: "FAIL", INFO: "info"}

LEGEND = {
    "detect": "disk read failures are surfaced to the user",
    "survive": "no userspace errors during a full read (survival)",
    "reboot": "the array assembles and root logs in after reboot",
    "clean": "previously detected errors clear after scrub",
    "rescue": "the root can be mounted and read from a livecd",
}


@dataclass
class Result:
    scenario: str
    check: str
    status: str
    detail: str = ""


@dataclass
class Report:
    stack: str
    level: str
    started: str
    # reproduction-relevant config fields (label, value), shown as a small table
    # at the top of the report so a result is self-describing.
    config_rows: list[tuple[str, str]] = field(default_factory=list)
    results: list[Result] = field(default_factory=list)
    # when set, the report is rewritten here after every check, so an interrupted
    # run (eg. ctrl-c) still leaves the partial record on disk.
    out_path: str | None = None
    # set only once the run reaches the end of its flow. until then the summary is
    # INCOMPLETE, so a report flushed by a killed/hung/aborted run is never
    # mistaken for a clean pass.
    completed: bool = False

    def add(self, scenario: str, check: str, status: str, detail: str = "") -> Result:
        r = Result(scenario, check, status, detail)
        self.results.append(r)
        self.flush()
        return r

    def mark_complete(self) -> None:
        self.completed = True
        self.flush()

    def flush(self) -> None:
        if not self.out_path:
            return
        try:
            with open(self.out_path, "w") as f:
                f.write(self.to_markdown())
        except OSError:
            pass

    def failed(self) -> list[Result]:
        return [r for r in self.results if r.status == FAIL]

    def to_markdown(self) -> str:
        lines = [
            f"# raiden vm e2e: {self.stack} ({self.level})",
            "",
            f"started: {self.started}",
            "",
        ]
        if self.config_rows:
            lines += ["configuration:", "", "| field | value |", "| --- | --- |"]
            lines += [f"| {k} | {v} |" for k, v in self.config_rows]
            lines += [""]
        lines += ["legend:", ""]
        lines += [f"- {k}: {v}" for k, v in LEGEND.items()]
        lines += ["", "| scenario | check | result | detail |", "| --- | --- | --- | --- |"]
        for r in self.results:
            detail = r.detail.replace("|", "\\|").replace("\n", " ")
            lines.append(f"| {r.scenario} | {r.check} | {MARK.get(r.status, r.status)} | {detail} |")
        n_fail = len(self.failed())
        if not self.completed:
            verdict = f"**INCOMPLETE** -- run did not finish; {n_fail} failing check(s) so far."
        elif n_fail:
            verdict = f"**FAILED** -- {n_fail} failing check(s)."
        else:
            verdict = "**OK** -- 0 failing check(s)."
        lines += ["", verdict]
        return "\n".join(lines) + "\n"
