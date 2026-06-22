"""automated libvirt vm end-to-end harness for raiden.

drives a real install and the corruption/repair scenarios over the serial
console (no human in the loop), grades each result, and writes a markdown
report. see README.md for requirements and usage."""

__all__ = ["config", "report", "domain", "console", "vm", "scenarios", "runner"]
