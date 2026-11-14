"""One shape for every tool bench/yardstick.py times, scrutate's own forms included.

An adapter answers one workload request at a time; the harness checks the answer, then times
`ask` (warm-up, interleaved runs). `start` builds the tool's index or launches its server and waits
until it answers; its cost is the tool's setup, reported apart from the query times.
"""
from __future__ import annotations

from dataclasses import dataclass


class Unsupported(Exception):
    """A request this tool has no form of (a server without call hierarchy): reported as missing."""


@dataclass
class Setup:
    seconds: float = 0.0
    peak_rss: int | None = None
    index_bytes: int | None = None


class Adapter:
    name = "?"
    workloads: frozenset = frozenset()
    # "process" starts a program per request; "resident" asks a server that holds its index
    form = "process"
    # the query languages it answers for (a language server); None for any
    languages: frozenset | None = None

    def missing(self) -> str | None:
        """Why the tool cannot run here (no binary, no corpus, no API key); None when it can."""
        return None

    def version(self) -> str:
        return "?"

    def start(self, corpus: str, work: str) -> Setup:
        return Setup()

    def ask(self, workload: str, query: dict):
        """One request; the answer normalised for the workload (see yardstick.check)."""
        raise NotImplementedError

    def stop(self) -> None:
        pass
