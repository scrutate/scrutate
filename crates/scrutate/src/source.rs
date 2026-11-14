//! How Engine A reads one byte source: the first read, the byte-order mark, the NUL byte, the size
//! thresholds, the split of a large file, the long line and the searcher's setup. A source is a
//! file on disk, standard input, an archive member or text held in memory; the rules ask it for
//! its size, positioned reads and one sequential read, and nothing else.

use super::*;
use std::fs::File;
use std::io::Read;

/// Reads at an offset, without moving the position a sequential read starts from.
pub(super) trait ReadAt {
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize>;
}

impl ReadAt for File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, off)
    }
    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, off)
    }
    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, _buf: &mut [u8], _off: u64) -> std::io::Result<usize> {
        Err(std::io::Error::other("positional reads unavailable"))
    }
}

/// What the read rules ask of a source.
pub(super) trait Source {
    type At: ReadAt + Sync;
    /// Its positioned reads, when it gives them.
    fn at(&self) -> Option<&Self::At>;
    /// Its size when it is known without asking the system.
    fn known_size(&self) -> Option<u64> {
        None
    }
    /// Its size, asked of the system when it is not known.
    fn size(&self) -> Option<u64> {
        self.known_size()
    }
    /// When it was opened: a small source that took long to read came from the disk.
    fn opened(&self) -> Option<Instant> {
        None
    }
    /// Its bytes come out of a decompressor, so one line may be gigabytes.
    fn expands(&self) -> bool {
        false
    }
    /// Search it as one stream from its start.
    fn stream<K: Sink>(&mut self, m: &AnyMatcher, s: &mut Searcher, sink: K) -> Result<(), K::Error>;
}

/// A file on disk.
pub(super) struct Disk {
    file: File,
    size: Option<u64>,
    positioned: bool,
    opened: Instant,
}

impl Disk {
    /// `size`: the walk's, when it has one. `positioned`: reads at an offset see the whole file,
    /// which they do not on a pseudo filesystem.
    pub(super) fn open(path: &Path, size: Option<u64>, positioned: bool) -> std::io::Result<Disk> {
        let opened = Instant::now();
        Ok(Disk { file: File::open(path)?, size, positioned, opened })
    }
}

impl Source for Disk {
    type At = File;
    fn at(&self) -> Option<&File> {
        self.positioned.then_some(&self.file)
    }
    fn known_size(&self) -> Option<u64> {
        self.size
    }
    fn size(&self) -> Option<u64> {
        self.size.or_else(|| self.file.metadata().ok().map(|m| m.len()))
    }
    fn opened(&self) -> Option<Instant> {
        Some(self.opened)
    }
    fn stream<K: Sink>(&mut self, m: &AnyMatcher, s: &mut Searcher, sink: K) -> Result<(), K::Error> {
        // a positioned read on Windows moves the file's cursor, and this search reads from it
        #[cfg(windows)]
        let _ = std::io::Seek::seek(&mut &self.file, std::io::SeekFrom::Start(0));
        m.search_file(s, &self.file, sink)
    }
}

/// Text held in memory.
#[derive(Clone, Copy)]
pub(super) struct Bytes<'b>(pub &'b [u8]);

impl ReadAt for Bytes<'_> {
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        let rest = usize::try_from(off).ok().and_then(|o| self.0.get(o..)).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

impl Source for Bytes<'_> {
    type At = Self;
    fn at(&self) -> Option<&Self> {
        Some(self)
    }
    fn known_size(&self) -> Option<u64> {
        Some(self.0.len() as u64)
    }
    fn stream<K: Sink>(&mut self, m: &AnyMatcher, s: &mut Searcher, sink: K) -> Result<(), K::Error> {
        m.search_reader(s, self.0, sink)
    }
}

/// The positioned reads of a source that has none.
pub(super) enum NoReads {}

impl ReadAt for NoReads {
    fn read_at(&self, _buf: &mut [u8], _off: u64) -> std::io::Result<usize> {
        match *self {}
    }
}

/// Standard input.
pub(super) struct Stdin;

impl Source for Stdin {
    type At = NoReads;
    fn at(&self) -> Option<&NoReads> {
        None
    }
    fn stream<K: Sink>(&mut self, m: &AnyMatcher, s: &mut Searcher, sink: K) -> Result<(), K::Error> {
        m.search_reader(s, std::io::stdin().lock(), sink)
    }
}

/// A member of an archive, or a decompressed file.
pub(super) struct Member<'r>(pub &'r mut dyn Read);

impl Source for Member<'_> {
    type At = NoReads;
    fn at(&self) -> Option<&NoReads> {
        None
    }
    fn expands(&self) -> bool {
        true
    }
    fn stream<K: Sink>(&mut self, m: &AnyMatcher, s: &mut Searcher, sink: K) -> Result<(), K::Error> {
        m.search_reader(s, &mut *self.0, sink)
    }
}

/// What one search thread keeps from source to source: its searchers and the buffer small files are
/// read into (a new zeroed buffer per file cost about 4% of the CPU of a Linux-tree search on Linux).
pub(super) struct FileWorker {
    searcher: Searcher,
    /// for files named on the command line, made on the first one
    named: Option<Searcher>,
    /// for the members of one archive, made on its first member
    pub(super) member: Option<Searcher>,
    pub(super) small: Vec<u8>,
    /// the plain output of a file before it is written
    out: Vec<u8>,
}

pub(super) fn file_worker(q: &Query) -> FileWorker {
    FileWorker { searcher: make_searcher(q, BinaryDetection::quit(0), None, true), named: None, member: None, small: Vec::new(), out: Vec::new() }
}

/// `heap`: the most bytes the searcher may hold, a line and its context (all of the input with -U).
/// `bom`: a byte-order mark at the start is transcoded, which only the start of a stream has.
pub(super) fn make_searcher(q: &Query, binary: BinaryDetection, heap: Option<usize>, bom: bool) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(binary)
        .bom_sniffing(bom)
        .heap_limit(heap)
        .line_number(q.report == Report::Lines) // line numbers cost a pass over the buffer
        .multi_line(q.pattern.multiline)
        .before_context(q.before)
        .after_context(q.after)
        .invert_match(q.invert)
        .max_matches(q.max_count)
        .build()
}

/// Files up to this size are read with one call (`first`) instead of reading until a zero-length
/// read; within it the searcher's binary check covers the whole file.
pub(super) const SMALL_FILE: u64 = 64 << 10;
/// Files up to this size are streamed by a searcher made for the file, larger ones by the
/// thread's. Reading them whole instead cost each thread a buffer the size of the largest such
/// file it met (py-big: 12.6 MB peak resident on 5 threads, 8.2 MB streamed) for 1% fewer
/// instructions at most (py-big and Linux).
const MID_FILE: u64 = 1 << 20;

/// Line-at-a-time searches whose results do not depend on neighbouring lines or on a running count.
/// `-l` stays sequential: it stops at the first match. On one thread a file is never split, but a
/// file of one long line is still read into a buffer of its size (`split`).
fn splittable(o: &Query) -> bool {
    !o.pattern.multiline && o.before == 0 && o.after == 0 && o.max_count.is_none() && o.max_results.is_none() && o.report != Report::Files
}

/// UTF-8 and UTF-16 byte-order marks: only the stream reader transcodes them.
fn starts_with_bom(b: &[u8]) -> bool {
    b.starts_with(&[0xEF, 0xBB, 0xBF]) || b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF])
}

/// Search `src`, reported as `rel`, and send what it finds. `named`: a root that is a file, or
/// standard input (`Files::paths_as_given`).
pub(super) fn search<S: Source>(w: &mut FileWorker, r: &Run, src: &mut S, rel: Arc<str>, named: bool) {
    let (o, f, c) = (r.q, r.f, r.c);
    // -U holds the whole member, so a limit would refuse large members that are fine
    let limit = (src.expands() && !o.pattern.multiline).then_some(crate::archive::MAX_LINE);
    let mut searcher = if src.expands() {
        let binary = if named { BinaryDetection::convert(0) } else { BinaryDetection::quit(0) };
        w.member.get_or_insert_with(|| make_searcher(o, binary, limit, true))
    } else if named {
        w.named.get_or_insert_with(|| make_searcher(o, BinaryDetection::convert(0), None, true))
    } else {
        &mut w.searcher
    };
    let mut fresh = None;
    let mut owned = None;
    let mut small = None;
    if let Some(at) = src.at().filter(|_| splittable(o)) {
        // most files are small: one read of up to SMALL_FILE + 1 bytes takes such a file whole and
        // tells a larger one apart, with no call for the size (a `statx` per file otherwise: 96,000
        // of them on the Linux tree)
        let first = if src.known_size().is_none_or(|n| n <= SMALL_FILE) { first(at, &mut w.small) } else { First::Larger };
        if let (First::Whole(_) | First::Other, Some(opened)) = (&first, src.opened()) {
            c.small_reads.fetch_add(1, Ordering::Relaxed);
            if r.p.slow(opened.elapsed().as_nanos() as u64) {
                c.slow_reads.fetch_add(1, Ordering::Relaxed);
            }
        }
        match first {
            First::Whole(n) => small = Some(n),
            First::Other => {}
            First::Larger => {
                let len = src.size().unwrap_or(0);
                let min = f.split_file_bytes;
                if len >= min {
                    match split(r, at, len, (min / 8).max(1), &rel) {
                        Outcome::Done => return,
                        Outcome::Stream => {}
                        Outcome::Buffer(b) => owned = Some(b),
                    }
                } else if len <= MID_FILE && r.p.range_threads() > 1 {
                    // a searcher of its own, so the thread's keeps the buffer size it had: where a
                    // NUL byte after the first fill stops the output depends on that size. One
                    // thread meets the files in a fixed order and keeps its one searcher
                    searcher = fresh.insert(make_searcher(o, if named { BinaryDetection::convert(0) } else { BinaryDetection::quit(0) }, None, true));
                }
            }
        }
    }
    let whole: Option<&[u8]> = owned.as_deref().or(small.map(|n| &w.small[..n]));
    let mut sink = Collector { convert: named, out: std::mem::take(&mut w.out), ..r.collector(Arc::clone(&rel)) };
    let res = match whole {
        // only the stream reader turns NUL bytes into line breaks
        Some(b) if named && memchr::memchr(0, b).is_some() => r.matcher.search_reader(searcher, b, &mut sink),
        Some(b) => r.matcher.search_slice(searcher, b, &mut sink),
        None => src.stream(r.matcher, searcher, &mut sink),
    };
    match res {
        // grep-searcher's own words for the limit name no line
        Err(e) if limit.is_some() && e.to_string().starts_with("configured allocation limit") => {
            c.failed(format!("{rel}: a line longer than {} MiB; stopped", crate::archive::MAX_LINE >> 20))
        }
        Err(e) => c.failed(format!("{rel}: {e}")),
        Ok(()) => {}
    }
    w.out = report(r, sink, rel, named);
}

/// Bytes `pos..end` of a source, read without moving a shared position.
struct Range<'f, A> {
    at: &'f A,
    pos: u64,
    end: u64,
}

impl<A: ReadAt> Read for Range<'_, A> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.end {
            return Ok(0);
        }
        let want = buf.len().min((self.end - self.pos) as usize);
        let n = self.at.read_at(&mut buf[..want], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// The start of the first line beginning in `x..x + 64 KiB` (just past a newline), if any: a
/// longer line is left whole inside a bigger range rather than read twice to find its end.
fn line_start<A: ReadAt>(at: &A, x: u64, buf: &mut [u8]) -> std::io::Result<Option<u64>> {
    let n = at.read_at(buf, x - 1)?;
    Ok(memchr::memchr(b'\n', &buf[..n]).map(|i| x + i as u64))
}

/// Newlines in `a..b`; None when the range holds a NUL byte, a read fails or `stop` is set.
fn newlines<A: ReadAt>(at: &A, a: u64, b: u64, buf: &mut [u8], stop: &AtomicBool) -> Option<u64> {
    let mut r = Range { at, pos: a, end: b };
    let mut n = 0u64;
    loop {
        let got = r.read(buf).ok()?;
        if got == 0 {
            return Some(n);
        }
        if stop.load(Ordering::Relaxed) || memchr::memchr(0, &buf[..got]).is_some() {
            stop.store(true, Ordering::Relaxed);
            return None;
        }
        n += memchr::memchr_iter(b'\n', &buf[..got]).count() as u64;
    }
}

pub(super) enum Outcome {
    /// searched and reported
    Done,
    /// nothing reported: search the source as one stream
    Stream,
    /// nothing reported: the source has no line breaks to split at and no NUL byte, so here it is
    /// in one buffer of its exact size (a stream would triple its buffer, copying each time)
    Buffer(Vec<u8>),
}

/// What one read of a source's first `SMALL_FILE + 1` bytes found.
enum First {
    /// the whole source, this many bytes, in the buffer
    Whole(usize),
    /// more than `SMALL_FILE` bytes
    Larger,
    /// a small source for the stream reader: it starts with a byte-order mark, or the read failed
    Other,
}

/// Reads into `buf`, kept by the thread across files (sized once, never zeroed again).
fn first<A: ReadAt>(at: &A, buf: &mut Vec<u8>) -> First {
    let max = SMALL_FILE as usize;
    if buf.len() <= max {
        *buf = vec![0; max + 1];
    }
    match at.read_at(&mut buf[..max + 1], 0) {
        Ok(n) if n > max => First::Larger,
        Ok(n) if starts_with_bom(&buf[..n]) => First::Other,
        Ok(n) => First::Whole(n),
        Err(_) => First::Other,
    }
}

/// The whole source when it holds no NUL byte (a slice search checks only its start for binary
/// data), no line break but its last byte (lines too long for the probes to see go to the
/// stream, which holds a few times the longest line) and is still `size` bytes long. The threads
/// take 1 MiB pieces in order, so a line break stops them about one line plus a piece each
/// into the source. The time goes to the kernel copying pages into fresh memory, and that work
/// spreads as far as opening files does (100 MB on macOS: 12.6 ms on one thread, 4.0 ms on 5,
/// 6.7 ms on 11).
pub(super) fn whole<A: ReadAt + Sync>(at: &A, size: u64, threads: usize) -> Option<Vec<u8>> {
    const PIECE: usize = 1 << 20;
    let mut buf = vec![0u8; usize::try_from(size).ok()?];
    let stop = AtomicBool::new(false);
    {
        let pieces = std::sync::Mutex::new(buf.chunks_mut(PIECE).enumerate());
        let work = || {
            while !stop.load(Ordering::Relaxed) {
                let Some((i, b)) = pieces.lock().ok().and_then(|mut p| p.next()) else { return };
                if !fill(at, b, (i * PIECE) as u64, size) {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        };
        std::thread::scope(|s| {
            // an empty source has no piece, and clamp panics on a bound below 1
            for _ in 1..threads.clamp(1, size.div_ceil(PIECE as u64).max(1) as usize) {
                s.spawn(work);
            }
            work();
        });
    }
    (!stop.into_inner()).then_some(buf)
}

/// Fills `buf` from `off` in the source; false on a NUL byte, a line break before the source's
/// last byte, a failed read or a shorter source.
fn fill<A: ReadAt>(at: &A, buf: &mut [u8], off: u64, size: u64) -> bool {
    let mut done = 0;
    while done < buf.len() {
        match at.read_at(&mut buf[done..], off + done as u64) {
            Ok(n) if n > 0 => match memchr::memchr2(0, b'\n', &buf[done..done + n]) {
                Some(i) if buf[done + i] == 0 || off + (done + i) as u64 + 1 != size => return false,
                _ => done += n,
            },
            _ => return false,
        }
    }
    true
}

/// `whole`'s source searched for a plain literal when all the output needs is whether the line
/// matches and its text: a first pass reads it through a small buffer per thread, and a
/// matching line is read again as it is written. None when that does not apply; Stream when
/// the source is not of one line after all, with nothing reported.
fn one_line<A: ReadAt + Sync>(r: &Run, at: &A, size: u64, rel: &Arc<str>) -> Option<Outcome> {
    let (o, AnyMatcher::Literal(lit)) = (r.q, r.matcher) else { return None };
    // a shorter line goes to the collector's buffer, which needs it in memory
    if o.invert || !(r.plain.is_some() || o.report == Report::Counts) || size < (1 << 16) + 2 {
        return None;
    }
    #[cfg(test)]
    if tests::NO_ONE_LINE.with(|c| c.get()) {
        return None;
    }
    let Some(found) = literal_in(at, size, &lit.0, r.p.threads()) else { return Some(Outcome::Stream) };
    #[cfg(test)]
    tests::ONE_LINE.with(|n| n.set(n.get() + 1));
    if found {
        let mut end = [0u8; 2];
        let len = match at.read_at(&mut end, size - 2) {
            Ok(2) => match end {
                [b'\r', b'\n'] => size - 2,
                [_, b'\n'] => size - 1,
                _ => size,
            },
            _ => return Some(Outcome::Stream),
        };
        let mut sink = r.collector(Arc::clone(rel));
        if let Err(e) = sink.take_match(b"", Text::At(at, len), Some(1)) {
            r.c.failed(format!("{rel}: {e}"));
        }
        report(r, sink, Arc::clone(rel), false);
    }
    Some(Outcome::Done)
}

/// Whether `finder`'s needle is in a source that holds no NUL byte, no line break but its last
/// byte and is still `size` bytes long; None when it is not such a source or a read failed. The
/// threads take pieces in order, each read with the needle's length less one bytes of the
/// next, so a needle across two pieces is found. 100 MB on macOS, 5 threads: 256 KiB pieces
/// took 5.1 ms and 12.8 ms of CPU, 1 MiB pieces 8.0 and 20.0 (one fresh buffer: 14.5 and 32.2).
/// At most 8 threads: more read no faster, and `-j 64` held 22 MB.
fn literal_in<A: ReadAt + Sync>(at: &A, size: u64, finder: &memchr::memmem::Finder, threads: usize) -> Option<bool> {
    literal_in_pieces(at, size, finder, threads.min(8), 256 << 10)
}

pub(super) fn literal_in_pieces<A: ReadAt + Sync>(at: &A, size: u64, finder: &memchr::memmem::Finder, threads: usize, piece: usize) -> Option<bool> {
    let over = finder.needle().len() - 1;
    let pieces = size.div_ceil(piece as u64);
    let (next, stop, found) = (AtomicU64::new(0), AtomicBool::new(false), AtomicBool::new(false));
    let work = || {
        let mut buf = vec![0u8; piece + over];
        while !stop.load(Ordering::Relaxed) {
            let off = next.fetch_add(1, Ordering::Relaxed) * piece as u64;
            if off >= size {
                return;
            }
            let b = &mut buf[..(size - off).min((piece + over) as u64) as usize];
            if !fill(at, b, off, size) {
                stop.store(true, Ordering::Relaxed);
            } else if !found.load(Ordering::Relaxed) && finder.find(b).is_some() {
                found.store(true, Ordering::Relaxed);
            }
        }
    };
    std::thread::scope(|s| {
        for _ in 1..threads.clamp(1, pieces.max(1) as usize) {
            s.spawn(work);
        }
        work();
    });
    (!stop.into_inner()).then(|| found.into_inner())
}

struct Part {
    out: Vec<u8>,
    hits: Vec<Record>,
    matched: bool,
    count: u64,
    /// the search reached the end of the range
    ok: bool,
}

/// Search a source of `size` bytes as ranges of whole lines, `chunk` bytes or so each, on several
/// threads, and report the results in order exactly as one sequential pass would. A first pass
/// counts the newlines of every range (for the line numbers). Reports nothing when the source
/// must be searched sequentially after all: it starts with a byte-order mark (transcoded as one
/// stream), holds a NUL byte (binary detection), a read failed, or it has no line breaks to split at.
pub(super) fn split<A: ReadAt + Sync>(r: &Run, at: &A, size: u64, chunk: u64, rel: &Arc<str>) -> Outcome {
    let Run { q: o, p, c, stop, tx, plain, .. } = *r;
    let mut head = [0u8; 3];
    match at.read_at(&mut head, 0) {
        Ok(n) if starts_with_bom(&head[..n]) => return Outcome::Stream,
        Ok(_) => {}
        Err(_) => return Outcome::Stream,
    }
    // one thread only asks whether the source has a line break to split at: the stream holds 3^k
    // times 64 KiB for a long line (141.6 MB peak resident for one 100 MB line, 103.9 whole)
    let one = p.range_threads() == 1;
    let mut bounds = vec![0u64];
    let mut probe = vec![0u8; 64 << 10];
    let mut x = chunk;
    while x < size && !(one && bounds.len() > 1) {
        match line_start(at, x, &mut probe) {
            Ok(Some(b)) if b < size => {
                bounds.push(b);
                x = b + chunk;
            }
            Ok(_) => x += chunk,
            Err(_) => return Outcome::Stream,
        }
    }
    bounds.push(size);
    if bounds.len() < 3 {
        if let Some(done) = one_line(r, at, size, rel) {
            return done;
        }
        return whole(at, size, p.threads()).map_or(Outcome::Stream, Outcome::Buffer);
    }
    if one {
        return Outcome::Stream;
    }
    let ranges: Vec<(u64, u64)> = bounds.windows(2).map(|w| (w[0], w[1])).collect();
    let threads = p.range_threads().min(ranges.len());

    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let counted: Vec<Option<Vec<(usize, u64)>>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    // 1 MiB a thread held 15.2 MB peak resident on a 740 MB file (6.9 MB
                    // now), for 0.4% fewer instructions
                    let mut buf = vec![0u8; 256 << 10];
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(a, b)) = ranges.get(i) else { return Some(mine) };
                        mine.push((i, newlines(at, a, b, &mut buf, &failed)?));
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    // a NUL byte (binary handling stays sequential) or a read error: no line numbers to trust
    let Ok(per_thread) = counted.into_iter().map(|w| w.ok_or(())).collect::<Result<Vec<_>, ()>>() else {
        return Outcome::Stream;
    };
    let mut lines = vec![0u64; ranges.len()];
    for (i, n) in per_thread.concat() {
        lines[i] = n;
    }
    let mut bases = Vec::with_capacity(ranges.len());
    let mut acc = 0u64;
    for n in &lines {
        bases.push(acc);
        acc += n;
    }

    // one window of ranges at a time, so the output held back for ordering stays bounded
    let mut matched = false;
    let mut count = 0u64;
    'windows: for (rs, bs) in ranges.chunks(threads).zip(bases.chunks(threads)) {
        let parts: Vec<Part> = std::thread::scope(|s| {
            let hs: Vec<_> = rs.iter().zip(bs).map(|(&(a, b), &base)| s.spawn(move || search_range(r, at, a, b, base, rel))).collect();
            hs.into_iter().map(|h| h.join().unwrap_or(Part { out: Vec::new(), hits: Vec::new(), matched: false, count: 0, ok: false })).collect()
        });
        for part in parts {
            matched |= part.matched;
            count += part.count;
            if let Some(p) = plain {
                p.write(&part.out, false, stop);
            } else {
                let mut hits = part.hits.into_iter().peekable();
                while hits.peek().is_some() {
                    send(c, tx, hits.by_ref().take(512).collect());
                }
            }
            if !part.ok {
                // the source changed after the first pass, or a read failed: what was found so far stands
                c.failed(format!("{rel}: changed while it was searched, or a read failed"));
                break 'windows;
            }
        }
        if let Some(p) = plain {
            p.tick(stop);
        }
        if stop.is_set() {
            break;
        }
    }
    if matched {
        c.files_matched.fetch_add(1, Ordering::Relaxed);
        if o.report == Report::Counts {
            send(c, tx, vec![Record::Count { path: Arc::clone(rel), path_bytes: r.raw.cloned(), count }]);
        }
    }
    Outcome::Done
}

fn search_range<A: ReadAt>(r: &Run, at: &A, a: u64, b: u64, base: u64, rel: &Arc<str>) -> Part {
    let stop = r.stop;
    // its own matcher (its own regex cache pool) and its own searcher, without transcoding: the
    // middle of a source is not the start of a stream
    let matcher = r.matcher.clone();
    let mut searcher = make_searcher(r.q, BinaryDetection::quit(0), None, false);
    // with `hold` the plain text is formatted into `out` and never written to this sink
    let sink_plain = PlainSink::new(Box::new(std::io::sink()));
    let range_run = Run { matcher: &matcher, plain: if r.plain.is_some() { Some(&sink_plain) } else { None }, ..*r };
    // the records wait in `batch` for the range's turn: a bounded channel would wait for a reader
    let mut sink = Collector {
        tx: None,
        max_results: None,
        line_base: base,
        hold: true,
        multiline: false, // never split in multiline mode
        ..range_run.collector(Arc::clone(rel))
    };
    let mut range = Range { at, pos: a, end: b };
    let res = matcher.search_reader(&mut searcher, &mut range, &mut sink);
    sink.flush();
    let ok = res.is_ok() && !sink.binary && (range.pos >= b || stop.is_set());
    Part { out: std::mem::take(&mut sink.out), hits: std::mem::take(&mut sink.batch), matched: sink.matched, count: sink.file_count, ok }
}
