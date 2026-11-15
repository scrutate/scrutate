// Does a file's event always come before the event of a cookie made after the write returned?
// The cookie barrier (docs/adr/0001-change-sets-and-the-cookie-barrier.md) rests on it.
//
//   macOS: cc -O2 -o /tmp/event_order bench/event_order.c -framework CoreServices
//   Linux: cc -O2 -o /tmp/event_order bench/event_order.c
//   /tmp/event_order DIR ROUNDS trunc|rename [FLOOD_DIR]
//
// Each round writes DIR/src/w.py (trunc: open O_TRUNC, write, close, as std::fs::write does;
// rename: a temporary file renamed over it), creates DIR/cookies/cookie-N, waits for the cookie's
// event (macOS after FSEventStreamFlushSync), then 30 ms more for a late event of w.py. FLOOD_DIR
// starts a child that writes, renames and removes small files there, like a build. Prints how
// many rounds saw the cookie first, with the event ids where the system gives them.
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define ROOM 4096
static char file[ROOM + 64], cookie[ROOM + 64];

static void flood(const char *dir) {
    mkdir(dir, 0755);
    char p[ROOM + 64], t[ROOM + 128], buf[20000];
    memset(buf, 'x', sizeof buf);
    for (unsigned i = 0;; i++) {
        snprintf(p, sizeof p, "%s/f%u.o", dir, i % 500);
        snprintf(t, sizeof t, "%s.tmp", p);
        int fd = open(t, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        (void)!write(fd, buf, 100 + (i * 7919) % 19000);
        close(fd);
        rename(t, p);
        if (i % 7 == 0) {
            snprintf(p, sizeof p, "%s/f%u.o", dir, (i * 13) % 500);
            unlink(p);
        }
    }
}

static void write_file(const char *mode, int i) {
    char body[64];
    int len = snprintf(body, sizeof body, "def written_%d():\n    return %d\n", i, i);
    if (!strcmp(mode, "rename")) {
        char tmp[ROOM + 128];
        snprintf(tmp, sizeof tmp, "%s.tmp", file);
        int fd = open(tmp, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        (void)!write(fd, body, len);
        close(fd);
        rename(tmp, file);
    } else {
        int fd = open(file, O_WRONLY | O_TRUNC, 0644);
        (void)!write(fd, body, len);
        close(fd);
    }
}

#ifdef __APPLE__
#include <CoreServices/CoreServices.h>
#include <dispatch/dispatch.h>
static dispatch_semaphore_t got;
static volatile int saw_file, saw_cookie;
static volatile unsigned long long file_id, cookie_id;

static void cb(ConstFSEventStreamRef s, void *info, size_t n, void *paths, const FSEventStreamEventFlags *fl, const FSEventStreamEventId *ids) {
    char **p = paths;
    for (size_t i = 0; i < n; i++) {
        if (!strcmp(p[i], file)) {
            if (!saw_file) {
                saw_file = saw_cookie ? 2 : 1;
                file_id = ids[i];
            }
        }
        if (!saw_cookie && !strcmp(p[i], cookie)) {
            saw_cookie = 1;
            cookie_id = ids[i];
            dispatch_semaphore_signal(got);
        }
    }
}
#else
#include <poll.h>
#include <sys/inotify.h>
static int in_fd;
#endif

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s DIR ROUNDS trunc|rename [FLOOD_DIR]\n", argv[0]);
        return 2;
    }
    char root[ROOM], src[ROOM + 16], cookies[ROOM + 16];
    mkdir(argv[1], 0755);
    if (!realpath(argv[1], root)) return 2;
    snprintf(src, sizeof src, "%s/src", root);
    snprintf(cookies, sizeof cookies, "%s/cookies", root);
    mkdir(src, 0755);
    mkdir(cookies, 0755);
    snprintf(file, sizeof file, "%s/w.py", src);
    int fd = open(file, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    (void)!write(fd, "x", 1);
    close(fd);
    int rounds = atoi(argv[2]);
    const char *mode = argv[3];
    pid_t flooder = 0;
    if (argc > 4 && (flooder = fork()) == 0) flood(argv[4]);
#ifdef __APPLE__
    got = dispatch_semaphore_create(0);
    CFStringRef cs = CFStringCreateWithCString(NULL, root, kCFStringEncodingUTF8);
    CFArrayRef arr = CFArrayCreate(NULL, (const void **)&cs, 1, &kCFTypeArrayCallBacks);
    FSEventStreamRef s = FSEventStreamCreate(NULL, cb, NULL, arr, kFSEventStreamEventIdSinceNow, 0.005, kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer);
    FSEventStreamSetDispatchQueue(s, dispatch_queue_create("event_order", NULL));
    FSEventStreamStart(s);
#else
    in_fd = inotify_init1(IN_NONBLOCK);
    int wsrc = inotify_add_watch(in_fd, src, IN_CLOSE_WRITE | IN_MODIFY | IN_MOVED_TO | IN_CREATE);
    int wck = inotify_add_watch(in_fd, cookies, IN_CREATE);
#endif
    usleep(300000);
    int first = 0, later = 0;
    for (int i = 0; i < rounds; i++) {
        char name[64];
        snprintf(name, sizeof name, "cookie-%d", i);
        snprintf(cookie, sizeof cookie, "%s/%s", cookies, name);
#ifdef __APPLE__
        saw_file = saw_cookie = 0;
        write_file(mode, i);
        close(open(cookie, O_CREAT | O_WRONLY, 0600));
        FSEventStreamFlushSync(s);
        dispatch_semaphore_wait(got, dispatch_time(DISPATCH_TIME_NOW, 2000000000LL));
        int before = saw_file == 1;
        usleep(30000);
        if (!before) {
            first++;
            later += saw_file == 2;
            printf("  round %d: cookie id %llu, file id %llu\n", i, cookie_id, file_id);
        }
#else
        write_file(mode, i);
        close(open(cookie, O_CREAT | O_WRONLY, 0600));
        int saw_file = 0, saw_cookie = 0;
        struct timespec t0, t;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        for (;;) {
            struct pollfd p = {in_fd, POLLIN, 0};
            poll(&p, 1, 30);
            char buf[65536] __attribute__((aligned(8)));
            ssize_t n;
            while ((n = read(in_fd, buf, sizeof buf)) > 0) {
                for (char *q = buf; q < buf + n;) {
                    struct inotify_event *e = (struct inotify_event *)q;
                    if (e->wd == wsrc && e->len && !strcmp(e->name, "w.py")) if (!saw_file) saw_file = saw_cookie ? 2 : 1;
                    if (e->wd == wck && e->len && !strcmp(e->name, name)) saw_cookie = 1;
                    if (e->mask & IN_Q_OVERFLOW) printf("  round %d: queue overflow\n", i);
                    q += sizeof *e + e->len;
                }
            }
            clock_gettime(CLOCK_MONOTONIC, &t);
            double ms = (t.tv_sec - t0.tv_sec) * 1e3 + (t.tv_nsec - t0.tv_nsec) / 1e6;
            if ((saw_cookie && ms > 30) || ms > 2000) break;
        }
        if (saw_file != 1) {
            first++;
            later += saw_file == 2;
            printf("  round %d: cookie first (file event %s)\n", i, saw_file == 2 ? "later" : "never");
        }
#endif
        unlink(cookie);
    }
    if (flooder) kill(flooder, SIGKILL);
    printf("%s, %d rounds: the cookie came first %d times (%d of them saw the file's event later)\n", mode, rounds, first, later);
    return 0;
}
