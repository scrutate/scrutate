/* Floor for a search that walks: list a tree on several threads and read every byte of every
 * regular file in it, doing nothing else. Hidden entries (a name that starts with ".") are
 * skipped and no ignore rule is read. A search that must find its files cannot be faster than
 * this on the same tree and cache state; bench/rawread.c, handed the list, can.
 *
 *   cc -O2 -pthread -o walkread bench/walkread.c
 *   walkread ROOT THREADS
 */
#include <dirent.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Directories waiting to be listed. A thread waits while the queue is empty and another thread
 * is still listing, since that one may add more. */
static char **queue;
static size_t qhead, qtail, qcap;
static int listing;
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static atomic_ullong total, nfiles;

static void push(char *dir) {
    pthread_mutex_lock(&mu);
    if (qtail == qcap) {
        qcap = qcap ? qcap * 2 : 4096;
        queue = realloc(queue, qcap * sizeof *queue);
    }
    queue[qtail++] = dir;
    pthread_cond_signal(&cv);
    pthread_mutex_unlock(&mu);
}

static char *pop(void) {
    pthread_mutex_lock(&mu);
    while (qhead == qtail && listing > 0) pthread_cond_wait(&cv, &mu);
    char *dir = NULL;
    if (qhead < qtail) {
        dir = queue[qhead++];
        listing++;
    }
    pthread_mutex_unlock(&mu);
    return dir;
}

static void listed(void) {
    pthread_mutex_lock(&mu);
    if (--listing == 0 && qhead == qtail) pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&mu);
}

static void *worker(void *arg) {
    (void)arg;
    size_t cap = 256 * 1024;
    char *buf = malloc(cap);
    unsigned long long bytes = 0, files = 0;
    for (char *dir; (dir = pop()) != NULL; free(dir), listed()) {
        int dfd = open(dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        DIR *d = dfd < 0 ? NULL : fdopendir(dfd);
        if (!d) {
            if (dfd >= 0) close(dfd);
            continue;
        }
        for (struct dirent *e; (e = readdir(d)) != NULL;) {
            if (e->d_name[0] == '.') continue;
            int type = e->d_type;
            if (type == DT_UNKNOWN) {
                struct stat st;
                if (fstatat(dfd, e->d_name, &st, AT_SYMLINK_NOFOLLOW) != 0) continue;
                type = S_ISDIR(st.st_mode) ? DT_DIR : S_ISREG(st.st_mode) ? DT_REG : DT_UNKNOWN;
            }
            if (type == DT_DIR) {
                size_t n = strlen(dir) + strlen(e->d_name) + 2;
                char *sub = malloc(n);
                snprintf(sub, n, "%s/%s", dir, e->d_name);
                push(sub);
            } else if (type == DT_REG) {
                /* by the directory's descriptor, so each open looks up one name */
                int fd = openat(dfd, e->d_name, O_RDONLY | O_CLOEXEC);
                if (fd < 0) continue;
                ssize_t n;
                while ((n = read(fd, buf, cap)) > 0) bytes += (unsigned long long)n;
                close(fd);
                files++;
            }
        }
        closedir(d);
    }
    atomic_fetch_add(&total, bytes);
    atomic_fetch_add(&nfiles, files);
    free(buf);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: walkread ROOT THREADS\n");
        return 2;
    }
    int threads = atoi(argv[2]);
    if (threads < 1) threads = 1;
    push(strdup(argv[1]));
    pthread_t *t = malloc((size_t)threads * sizeof(pthread_t));
    for (int i = 0; i < threads; i++) pthread_create(&t[i], NULL, worker, NULL);
    for (int i = 0; i < threads; i++) pthread_join(t[i], NULL);
    printf("%llu bytes in %llu files\n", (unsigned long long)atomic_load(&total), (unsigned long long)atomic_load(&nfiles));
    return 0;
}
