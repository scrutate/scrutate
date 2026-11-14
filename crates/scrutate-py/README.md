# scrutate (Python bindings)

Python access to the Scrutate engines through PyO3, built with maturin against the stable ABI:
per operating system and architecture, one `abi3` wheel for standard CPython 3.9 to 3.14, one
`abi3t` wheel for 3.15 and newer (standard and free-threaded), and a `cp314t` wheel for
free-threaded 3.14.

```python
import scrutate

# Engine A: index-free search, streamed as the files are scanned
for m in scrutate.grep("refresh_token", ["."], mode="literal"):
    print(m.path, m.line, m.text)

# Engine B: the persistent code-intelligence index
repo = scrutate.Repo(".")
repo.index()                       # incremental; releases the GIL while it runs
for hit in repo.search("where do we refresh expired tokens", k=5):
    print(hit.path, hit.start, hit.qualname, hit.score)
```

The GIL is released while indexing and searching, so several Python threads can drive the engines
in parallel. `grep` returns an iterator fed by a background search thread, which converts results
to Python objects one batch at a time.

`grep` checks its arguments when called: `ValueError` for a bad pattern, glob or file type or no
paths, `FileNotFoundError` for a missing path; `threads=` is capped at 256. Matches come in the
order the files are searched; `sort=True` gives the order of `rg --sort path` and the CLI's
`--sort` (`a/b` before `a-b`, then by line), and the first match then waits for the whole
search. With `max_results=N` it is the first N matches in that order, the same on every run, and
the search stops once they are in. `Repo.read()` raises `FileNotFoundError` for a missing file,
`ValueError` for a path outside the repository and `OSError` otherwise. Ctrl-C stops a `scrutate.grep(...)` or
a `Repo.index()` within about 100 ms; an interrupted update stops at its next batch of files and
leaves the index as it was. A panic inside the engine is raised as an exception: the `Repo` stays
usable, and an update that panicked has rolled back.

`Repo.symbol`, `Repo.callers`, `Repo.duplicates` and the hits of `Repo.callees` come as a
`scrutate.Results`: a `list` with one more field, `stopped_at`, the limit when more results were
left, else `None`. In `Repo.callers`, `undecided` counts the callers the resolver kept without
knowing whether they call this definition or another of the same name.

Every `Repo` method runs the operation the CLI and the MCP tools run, with the same defaults, the
same check of the files an answer cites, and the same recovery from a damaged index.
`Repo.references(name, max_results=200, textual=False, definition=None)` gives the `references`
answer as a dict (`scrutate refs --json`'s `result`). `Repo.call(tool, args, max_bytes=None,
offset=0)` runs any MCP tool by its name and arguments, `grep_code` with a `mode`, a `path` and
`globs` included, and returns the tool's envelope as a dict: `schema`, `result`, `stopped_at`
when the answer stopped at its limit, and `text`, the text form from item `offset` on within
`max_bytes`. `GrepIter.stats()` lists in `notes` the (path, text) of each file whose search a
NUL byte stopped after a match; a match after that byte is not in the results.
