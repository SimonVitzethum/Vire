/* peakrss CMD [ARGS...] — run CMD, print its peak resident set size in KB (one line)
   to stderr, forward its stdout. Portable RSS measurement (no /usr/bin/time). */
#include <stdio.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/resource.h>
int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: peakrss CMD [ARGS]\n"); return 2; }
    pid_t pid = fork();
    if (pid == 0) { execvp(argv[1], argv + 1); _exit(127); }
    int status; struct rusage ru;
    wait4(pid, &status, 0, &ru);
    fprintf(stderr, "%ld\n", ru.ru_maxrss);   /* KB on Linux */
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
