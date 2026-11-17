scrutate-ignore
===============
A modified copy of [`ignore`](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore)
0.4.34 by Andrew Gallant (BurntSushi), published under its own name so that
[scrutate](https://github.com/scrutate/scrutate) can be installed from crates.io. It keeps the
upstream licence, MIT OR Unlicense, and the library is still named `ignore`. Use upstream `ignore`
in your own projects.

The eleven changes from upstream:

1. The global git excludes path is parsed without compiling a regex (`excludes_file_span`).
2. An idle walker thread waits 10 us at first, doubling up to the original 1 ms, before looking
   for work again (`Worker::get_work`).
3. Gitignore lines of three common shapes are matched without a regex (`gitignore::Simple`), and a
   matcher used often compiles all its globs into one set (`Full`).
4. A parallel walk can start more threads while it runs (`WalkBuilder::grow_threads`).
5. The global git excludes file is read only when a walk meets a repository
   (`IgnoreInner::git_global`).
6. On Unix, a walk of `.` takes the current directory it was given instead of looking it up again
   (`WalkBuilder::process_current_dir`); elsewhere it still canonicalizes the path.
7. A walker thread that is paused sleeps a thirty-second of the time it has been paused, 50 us to
   1 ms (`pause`).
8. The rare error of a queued entry is boxed, so an entry is 96 bytes (`DirEntry.err`).
9. A walk on one thread queues its entries in a plain vector.
10. On wasi, where no thread starts, a parallel walk runs its one worker on the calling thread
    (`WalkParallel::visit`).
11. On a platform that is neither Unix nor Windows, such as wasi, an entry is built from the
    directory entry or the file's metadata where upstream returns an error
    (`DirEntryRaw::from_path`).

The upstream README follows.

ignore
======
The ignore crate provides a fast recursive directory iterator that respects
various filters such as globs, file types and `.gitignore` files. This crate
also provides lower level direct access to gitignore and file type matchers.

[![Build status](https://github.com/BurntSushi/ripgrep/workflows/ci/badge.svg)](https://github.com/BurntSushi/ripgrep/actions)
[![](https://img.shields.io/crates/v/ignore.svg)](https://crates.io/crates/ignore)

Dual-licensed under MIT or the [UNLICENSE](https://unlicense.org/).

### Documentation

[https://docs.rs/ignore](https://docs.rs/ignore)

### Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
ignore = "0.4"
```

### Example

This example shows the most basic usage of this crate. This code will
recursively traverse the current directory while automatically filtering out
files and directories according to ignore globs found in files like
`.ignore` and `.gitignore`:


```rust,no_run
use ignore::Walk;

for result in Walk::new("./") {
    // Each item yielded by the iterator is either a directory entry or an
    // error, so either print the path or the error.
    match result {
        Ok(entry) => println!("{}", entry.path().display()),
        Err(err) => println!("ERROR: {}", err),
    }
}
```

### Example: advanced

By default, the recursive directory iterator will ignore hidden files and
directories. This can be disabled by building the iterator with `WalkBuilder`:

```rust,no_run
use ignore::WalkBuilder;

for result in WalkBuilder::new("./").hidden(false).build() {
    println!("{:?}", result);
}
```

See the documentation for `WalkBuilder` for many other options.
