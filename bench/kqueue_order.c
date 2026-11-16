// Per-file kqueue as a freshness proof (crates/scrutate/src/kq.rs, docs/adr/0001-*): is a write's
// note already queued when the write returns? macOS only.
//
//   cc -O2 -o /tmp/kqueue_order bench/kqueue_order.c
//   /tmp/kqueue_order TREE ROUNDS trunc|rename [FLOOD_DIR]
//
// TREE is changed in place (random files rewritten): give it a copy.
// Opens every file and directory of TREE with O_EVTONLY, registers EVFILT_VNODE on each, then
// per round writes one random file (in place, or a temporary file renamed over it), and at once
// drains the queue with a zero timeout: the file's (or, for a rename, its directory's) event must
// be in that drain. Prints misses, the setup cost and the drain cost.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <fts.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/event.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <time.h>
#include <unistd.h>

#define MAXN 200000
static char *paths[MAXN];
static int isdir[MAXN], parent[MAXN], fds[MAXN], n;

static double now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e6 + t.tv_nsec / 1e3;
}

static void flood(const char *dir) {
    mkdir(dir, 0755);
    char p[4200], t[4300], buf[20000];
    memset(buf, 'x', sizeof buf);
    for (unsigned i = 0;; i++) {
        snprintf(p, sizeof p, "%s/f%u.o", dir, i % 500);
        snprintf(t, sizeof t, "%s.tmp", p);
        int fd = open(t, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        (void)!write(fd, buf, 100 + (i * 7919) % 19000);
        close(fd);
        rename(t, p);
    }
}

int main(int argc, char **argv) {
    int rounds = atoi(argv[2]);
    const char *mode = argv[3];
    struct rlimit rl;
    getrlimit(RLIMIT_NOFILE, &rl);
    int maxproc = 0;
    size_t sz = sizeof maxproc;
    sysctlbyname("kern.maxfilesperproc", &maxproc, &sz, NULL, 0);
    rl.rlim_cur = rl.rlim_max < (rlim_t)maxproc ? rl.rlim_max : (rlim_t)maxproc;
    setrlimit(RLIMIT_NOFILE, &rl);
    getrlimit(RLIMIT_NOFILE, &rl);
    printf("kern.maxfilesperproc %d, RLIMIT_NOFILE now %llu\n", maxproc, (unsigned long long)rl.rlim_cur);
    pid_t flooder = 0;
    if (argc > 4 && (flooder = fork()) == 0) flood(argv[4]);

    char *roots[] = {argv[1], NULL};
    double t0 = now_us();
    FTS *f = fts_open(roots, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
    FTSENT *e;
    int stack[4096];
    while ((e = fts_read(f)) && n < MAXN) {
        if (e->fts_info == FTS_DP) continue;
        if (e->fts_info != FTS_F && e->fts_info != FTS_D) continue;
        paths[n] = strdup(e->fts_path);
        isdir[n] = e->fts_info == FTS_D;
        parent[n] = e->fts_level > 0 ? stack[e->fts_level - 1] : -1;
        if (isdir[n]) stack[e->fts_level] = n;
        n++;
    }
    fts_close(f);
    int kq = kqueue();
    struct kevent *ch = calloc(n, sizeof *ch);
    int opened = 0;
    for (int i = 0; i < n; i++) {
        fds[i] = open(paths[i], O_EVTONLY);
        if (fds[i] < 0) { printf("open stops at %d of %d: %s\n", i, n, strerror(errno)); break; }
        EV_SET(&ch[i], fds[i], EVFILT_VNODE, EV_ADD | EV_CLEAR, NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_DELETE | NOTE_RENAME | NOTE_LINK | NOTE_REVOKE, 0, (void *)(long)i);
        opened++;
    }
    if (kevent(kq, ch, opened, NULL, 0, NULL) < 0) { perror("kevent register"); return 1; }
    double t1 = now_us();
    printf("%d entries (%d registered) in %.1f ms\n", n, opened, (t1 - t0) / 1000);

    struct kevent out[4096];
    struct timespec zero = {0, 0};
    int miss = 0, files = 0;
    int *fidx = calloc(n, sizeof *fidx);
    for (int i = 0; i < opened; i++) if (!isdir[i]) fidx[files++] = i;
    double drain_total = 0, drain_max = 0;
    srand(7);
    for (int r = 0; r < rounds; r++) {
        // empty the queue first, so the drain below holds only this round's events
        while (kevent(kq, NULL, 0, out, 4096, &zero) > 0) {}
        int i = fidx[rand() % files];
        char body[64];
        int len = snprintf(body, sizeof body, "round %d\n", r);
        int want = i;
        if (!strcmp(mode, "rename")) {
            char tmp[4200];
            snprintf(tmp, sizeof tmp, "%s.kqtmp", paths[i]);
            int fd = open(tmp, O_CREAT | O_WRONLY | O_TRUNC, 0644);
            (void)!write(fd, body, len);
            close(fd);
            rename(tmp, paths[i]);
            want = parent[i];  // the directory's entries changed; the old file's fd sees NOTE_DELETE
            // the renamed-over file is a new inode: watch it from now on
            close(fds[i]);
            fds[i] = open(paths[i], O_EVTONLY);
            struct kevent c;
            EV_SET(&c, fds[i], EVFILT_VNODE, EV_ADD | EV_CLEAR, NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_DELETE | NOTE_RENAME | NOTE_LINK | NOTE_REVOKE, 0, (void *)(long)i);
            kevent(kq, &c, 1, NULL, 0, NULL);
        } else {
            int fd = open(paths[i], O_WRONLY | O_TRUNC);
            (void)!write(fd, body, len);
            close(fd);
        }
        double d0 = now_us();
        int got = 0, k;
        while ((k = kevent(kq, NULL, 0, out, 4096, &zero)) > 0)
            for (int j = 0; j < k; j++) if ((long)out[j].udata == want) got = 1;
        double d = now_us() - d0;
        drain_total += d;
        if (d > drain_max) drain_max = d;
        if (!got) { miss++; printf("  round %d: no event for %s\n", r, paths[want]); }
    }
    if (flooder) kill(flooder, SIGKILL);
    printf("%s, %d rounds: %d missed; drain %.1f us mean, %.1f us max\n", mode, rounds, miss, drain_total / rounds, drain_max);
    return 0;
}
