/* SPDX-License-Identifier: MIT */
/*
 * SPROCKET CLIENT (cli.h). The same commands, messages and listings as
 * the Linux client, with plain line input. get copies remote files into
 * the local directory and put copies local files to the remote one; both
 * refuse to replace a file without -f, and a batch stops at the first file
 * that fails. Received files are written as NAME.$$$ and renamed into
 * place once complete, with the sender's modification time.
 *
 * The client sends one request at a time and prints only between them,
 * so the screen never holds up the serial port.
 */
#include <signal.h>
#include <stdio.h>
#include <string.h>

#ifdef __WATCOMC__
#include <direct.h>
#else
#include <unistd.h>
#endif

#include "cli.h"
#include "crc32.h"
#include "fsys.h"
#include "port.h"
#include "proto.h"
#include "req.h"
#include "shell.h"

#define HELLO_WAIT 60
#define LINE_MAX 128
#define SCREEN_WIDTH 80
/* How deep ls -R goes, as on Linux for a tree that cannot tell loops. */
#define MAX_DEPTH 32
#define FLAG_REPLACE 0x01
#define FLAG_MORE 0x01
/* Besides the REQ_ results: the listing arena is full. */
#define LIST_FULL 100

static const char footer[] = "Backspace erases, Esc clears the line, and Ctrl-D ends the session.\n"
                             "Esc or Ctrl-C stops a long listing or a transfer.\n";

static struct req_server server;
static volatile sig_atomic_t broken;
/* Set when the link failed; the session cannot be used again. */
static int lost;

/* Remote directory: a tree path without a leading /. */
static char rcwd[SHELL_PATH_MAX + 1];
/* Local directory, and where the program started (lcd with no DIR). */
static char lcwd[FS_PATH_MAX + 1];
static char home[FS_PATH_MAX + 1];

/* Paths being walked by ls -R, and the prefix shown before each name. */
static char walk_path[FS_PATH_MAX + 1];
static char walk_prefix[SHELL_PATH_MAX + 2];

static void on_break(int sig)
{
    broken = 1;
    signal(sig, on_break);
}

/* Whether the user asked to stop: Esc, Ctrl-C or Ctrl-Break. */
static int stop(void)
{
    if (broken) {
        broken = 0;
        return 1;
    }
    return port_escape();
}

static void put16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)(v >> 8);
}

static void put32(uint8_t *p, uint32_t v)
{
    put16(p, (uint16_t)(v & 0xFFFF));
    put16(p + 2, (uint16_t)(v >> 16));
}

static uint32_t get32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

static int is_sep(char ch)
{
    return ch == '/' || ch == '\\';
}

/*
 * Remote requests. Each returns REQ_OK or why not; the message for the
 * user comes from failure().
 */

/* Put a path at p; returns the bytes used. */
static uint16_t put_path(uint8_t *p, const char *path)
{
    size_t len = strlen(path);

    p[0] = (uint8_t)len;
    memcpy(p + 1, path, len);
    return (uint16_t)(1 + len);
}

/* STAT: the entry's kind and size, and its path as spelled there. */
static int remote_stat(const char *path, uint8_t *dir, uint32_t *size, char *spelled)
{
    const uint8_t *r;
    uint16_t len;
    int rc = req_call(MSG_STAT, put_path(req_payload(), path), &r, &len);

    if (rc != REQ_OK) {
        return rc;
    }
    if (len < 11 || r[0] < 10 || (uint16_t)(1 + r[0]) > len || r[10] > r[0] - 10
        || r[10] > SHELL_PATH_MAX) {
        return REQ_BAD;
    }
    *dir = r[1];
    *size = get32(r + 2);
    memcpy(spelled, r + 11, r[10]);
    spelled[r[10]] = '\0';
    return REQ_OK;
}

/* DIR_LIST: every entry of a directory, collected above mark. */
static int remote_list(const char *path, uint16_t mark)
{
    const uint8_t *r;
    uint16_t len;
    uint16_t index = 0;
    uint16_t at;
    uint8_t count;
    uint8_t i;
    uint8_t *p;
    int rc;

    for (;;) {
        p = req_payload();
        put16(p, index);
        rc = req_call(MSG_DIR_LIST, (uint16_t)(2 + put_path(p + 2, path)), &r, &len);
        if (rc != REQ_OK) {
            return rc;
        }
        if (len < 2) {
            return REQ_BAD;
        }
        count = r[1];
        for (i = 0, at = 2; i < count; i++) {
            if (at >= len || r[at] < 10 || (uint16_t)(at + 1 + r[at]) > len
                || r[at + 10] > r[at] - 10) {
                return REQ_BAD;
            }
            if (!list_entry(mark, (const char *)r + at + 11, r[at + 10], r[at + 1],
                            get32(r + at + 2))) {
                return LIST_FULL;
            }
            at = (uint16_t)(at + 1 + r[at]);
        }
        index = (uint16_t)(index + count);
        if (!(r[0] & FLAG_MORE)) {
            return REQ_OK;
        }
        if (count == 0) {
            return REQ_BAD;
        }
    }
}

/* The words for a failed request, as the Linux client shows them. */
static const char *failure(int rc)
{
    static char text[96];

    switch (rc) {
    case REQ_LOST:
        lost = 1;
        return "no response from the server";
    case REQ_CANCELLED:
        return "cancelled";
    case REQ_BAD:
        return "the server sent a response that does not parse";
    case LIST_FULL:
        return "too many names to list here";
    default:
        break;
    }
    switch (req_class) {
    case ERR_NOT_FOUND:
        return "No such file or directory";
    case ERR_NOT_A_DIRECTORY:
        return "Not a directory";
    case ERR_IS_A_DIRECTORY:
        return "Is a directory";
    case ERR_EXISTS:
        return "File exists";
    case ERR_READ_ONLY:
        return "the server is read-only";
    case ERR_AMBIGUOUS:
        return "several names differ only in case; type the exact name";
    case ERR_BAD_NAME:
        return "the server cannot hold this name";
    case ERR_VERIFY_FAILED:
        return "verification failed";
    default:
        break;
    }
    if (req_message[0] != '\0') {
        return req_message;
    }
    if (req_code != 0) {
        sprintf(text, "error class %u (code 0x%04x)", req_class, req_code);
    } else {
        sprintf(text, "error class %u", req_class);
    }
    return text;
}

static void say_failure(const char *what, int rc)
{
    char name[SHELL_PATH_MAX + 1];

    shell_ascii(what, name, sizeof name);
    printf("%s: %s\n", name, failure(rc));
}

/*
 * Local paths. The local directory is kept here rather than with chdir,
 * as a native path: C:\GAMES on DOS.
 */

/* Length of the root at the start of a native path: C:\ or /. */
static size_t root_len(const char *path)
{
#ifdef __WATCOMC__
    if (path[0] != '\0' && path[1] == ':') {
        return is_sep(path[2]) ? 3 : 2;
    }
#endif
    return is_sep(path[0]) ? 1 : 0;
}

/* Append a separator and name to a native path; 0 if it does not fit. */
static int join(char *path, const char *name, size_t len)
{
    size_t have = strlen(path);

    if (have + 1 + len > FS_PATH_MAX) {
        return 0;
    }
    if (have > 0 && !is_sep(path[have - 1]) && path[have - 1] != ':') {
        path[have++] = FS_SEP;
    }
    memcpy(path + have, name, len);
    path[have + len] = '\0';
    return 1;
}

/*
 * Resolve a typed local path against the local directory: / or \ start at
 * the root of its drive, a drive letter at that drive's root, ~ at home,
 * and .. stops at the root. Returns 0 if the result does not fit.
 */
static int local_target(const char *arg, char *out)
{
    const char *p = arg;
    size_t n;
    size_t root;
    size_t len;

    if (p[0] == '~' && (p[1] == '\0' || is_sep(p[1]))) {
        strcpy(out, home);
        p++;
    } else if (root_len(p) > 0) {
        n = root_len(p);
        memcpy(out, p, n);
        out[n] = '\0';
#ifdef __WATCOMC__
        if (n == 2) {
            /* D: means the root of D: here. */
            strcat(out, "\\");
        }
#endif
        p += n;
    } else {
        strcpy(out, lcwd);
    }
    if (is_sep(p[0]) && root_len(out) > 0) {
        out[root_len(out)] = '\0';
    }
    root = root_len(out);
    for (; *p; p += n + (p[n] != '\0')) {
        n = strcspn(p, "/\\");
        if (n == 0 || (n == 1 && p[0] == '.')) {
            continue;
        }
        if (n == 2 && p[0] == '.' && p[1] == '.') {
            len = strlen(out);
            while (len > root && !is_sep(out[len - 1])) {
                len--;
            }
            if (len > root) {
                len--;
            }
            out[len] = '\0';
            continue;
        }
        if (!join(out, p, n)) {
            return 0;
        }
    }
    return 1;
}

/* Collect a local directory's entries above mark, as remote_list. */
static int local_list(const char *path, uint16_t mark)
{
    struct fs_entry e;
    uint16_t index;
    int rc;

    for (index = 0;; index++) {
        rc = fs_scan(path, index, &e);
        if (rc == FS_END) {
            return REQ_OK;
        }
        if (rc != FS_OK) {
            req_class = ERR_NOT_FOUND;
            req_message[0] = '\0';
            return REQ_REFUSED;
        }
        if (!list_entry(mark, e.name, (uint8_t)strlen(e.name), e.dir, e.size)) {
            return LIST_FULL;
        }
    }
}

/* A directory's entries in either tree. */
static int list_dir(int remote, const char *path, uint16_t mark)
{
    return remote ? remote_list(path, mark) : local_list(path, mark);
}

/* A local path that does not exist, as a request would fail. */
static int missing(void)
{
    req_class = ERR_NOT_FOUND;
    req_message[0] = '\0';
    return REQ_REFUSED;
}

/*
 * Listings, for ls and lls. remote says which tree; walk_path holds the
 * directory and walk_prefix the names shown before its entries. Returns
 * 1, 0 if the user stopped it, or -1 with the reason in *rc.
 */
static int walk(int remote, int recursive, unsigned depth, int *rc)
{
    uint16_t mark = list_mark();
    uint16_t at;
    const char *name;
    uint8_t len;
    uint8_t dir;
    uint32_t size;
    size_t path_len = strlen(walk_path);
    size_t prefix_len = strlen(walk_prefix);
    int result = 1;

    *rc = list_dir(remote, walk_path, mark);
    if (*rc != REQ_OK) {
        list_release(mark);
        return -1;
    }
    at = list_top();
    while (result > 0 && list_next(mark, &at, &name, &len, &dir, &size)) {
        if (prefix_len + len + 1 >= sizeof walk_prefix) {
            continue;
        }
        memcpy(walk_prefix + prefix_len, name, len);
        walk_prefix[prefix_len + len] = '\0';
        if (dir) {
            strcat(walk_prefix, "/");
        }
        if (!list_line(walk_prefix, dir, size)) {
            *rc = LIST_FULL;
            result = -1;
            break;
        }
        if (dir && recursive && depth + 1 < MAX_DEPTH) {
            if (stop()) {
                result = 0;
                break;
            }
            if (remote) {
                if (path_len + 1 + len > SHELL_PATH_MAX) {
                    continue;
                }
                if (path_len > 0) {
                    strcat(walk_path, "/");
                }
                strncat(walk_path, name, len);
            } else if (!join(walk_path, name, len)) {
                continue;
            }
            /* An unreadable subdirectory is shown but not entered. */
            if (walk(remote, recursive, depth + 1, rc) == 0) {
                result = 0;
            } else if (lost) {
                result = -1;
            }
            walk_path[path_len] = '\0';
        }
        walk_prefix[prefix_len] = '\0';
    }
    list_release(mark);
    return result;
}

/* ls [-R] [PATH] on either tree. */
static void ls(int remote, char *arg)
{
    int recursive = shell_flag(&arg, "-R");
    uint8_t dir = 0;
    uint32_t size = 0;
    int rc = REQ_OK;
    int result;
    struct fs_entry e;

    list_init();
    if (remote) {
        if (!shell_target(rcwd, arg, walk_path, SHELL_PATH_MAX + 1)) {
            rc = missing();
        } else {
            rc = remote_stat(walk_path, &dir, &size, walk_path);
        }
    } else if (!local_target(arg, walk_path) || fs_stat(walk_path, &e) != FS_OK) {
        rc = missing();
    } else {
        dir = e.dir;
        size = e.size;
    }
    if (rc != REQ_OK) {
        say_failure(arg, rc);
        return;
    }
    if (!dir) {
        list_line(arg, 0, size);
    } else {
        walk_prefix[0] = '\0';
        result = walk(remote, recursive, 0, &rc);
        if (result == 0) {
            printf("^C\n");
            return;
        }
        if (result < 0) {
            say_failure(arg, rc);
            return;
        }
    }
    if (!list_print(stop)) {
        printf("^C\n");
    }
}

/* cd [DIR] and lcd [DIR]. */
static void cd(char *arg)
{
    char path[SHELL_PATH_MAX + 1];
    uint8_t dir;
    uint32_t size;
    int rc;

    if (!shell_target(arg[0] ? rcwd : "", arg, path, sizeof path)) {
        printf("%s: the path is too long\n", arg);
        return;
    }
    rc = remote_stat(path, &dir, &size, path);
    if (rc != REQ_OK) {
        say_failure(arg, rc);
    } else if (!dir) {
        printf("%s: Not a directory\n", arg);
    } else {
        strcpy(rcwd, path);
    }
}

static void lcd(char *arg)
{
    char path[FS_PATH_MAX + 1];
    struct fs_entry e;

    if (!local_target(arg[0] ? arg : "~", path) || fs_stat(path, &e) != FS_OK) {
        printf("%s: No such file or directory\n", arg);
    } else if (!e.dir) {
        printf("%s: Not a directory\n", arg);
    } else {
        strcpy(lcwd, path);
    }
}

/*
 * Transfers.
 */

/* A transfer's progress on one line, as a percentage. */
static char progress_label[SHELL_PATH_MAX + 32];
static int progress_shown;
static uint32_t progress_start;

static void progress_begin(const char *counter, const char *verb, const char *name)
{
    char shown[SHELL_PATH_MAX + 1];

    shell_ascii(name, shown, sizeof shown);
    sprintf(progress_label, "%s%s %s", counter, verb, shown);
    printf("%s: ", progress_label);
    progress_shown = -1;
    progress_start = port_ticks();
}

static void progress_update(uint32_t done, uint32_t total)
{
    int percent = total ? (int)((unsigned long long)done * 100 / total) : 100;

    if (percent != progress_shown && done < total) {
        progress_shown = percent;
        printf("\r%s: %d%%", progress_label, percent);
        fflush(stdout);
    }
}

static void progress_done(uint32_t bytes)
{
    uint32_t elapsed = port_ticks() - progress_start;
    uint16_t rate_hz = port_ticks_per_second();
    unsigned long tenths = (unsigned long)((elapsed * 10UL + rate_hz / 2) / rate_hz);
    char rate[12];

    shell_human((uint32_t)((unsigned long long)bytes * rate_hz / (elapsed ? elapsed : 1)), rate);
    printf("\r%s: %lu %s in %lu.%lus (%s/s)\n", progress_label, (unsigned long)bytes,
           bytes == 1 ? "byte" : "bytes", tenths / 10, tenths % 10, rate);
}

/* "(2 of 5) " in a batch of several files, otherwise nothing. */
static void counter(char *out, unsigned i, unsigned n)
{
    if (n > 1) {
        sprintf(out, "(%u of %u) ", i + 1, n);
    } else {
        out[0] = '\0';
    }
}

/*
 * The files a get or put names, collected above mark: one file, or those
 * in a directory matching * and ? in the last part of arg. *dir is set to
 * their directory (a tree path or a native path). Returns the number of
 * files, or 0 after saying why there are none.
 */
static unsigned expand(int remote, const char *arg, uint16_t mark, char *dir)
{
    char path[FS_PATH_MAX + 1];
    char spelled[SHELL_PATH_MAX + 1];
    const char *pattern;
    const char *name;
    uint16_t at;
    uint8_t len;
    uint8_t is_dir;
    uint32_t size;
    unsigned count = 0;
    struct fs_entry e;
    int rc;

    if (remote ? !shell_target(rcwd, arg, path, SHELL_PATH_MAX + 1) : !local_target(arg, path)) {
        printf("%s: the path is too long\n", arg);
        return 0;
    }
    pattern = remote ? shell_base(path) : path + strlen(path);
    while (!remote && pattern > path + root_len(path) && !is_sep(pattern[-1])) {
        pattern--;
    }
    /* The directory part. */
    memcpy(dir, path, (size_t)(pattern - path));
    dir[pattern - path] = '\0';
    if (remote && dir[0] != '\0') {
        dir[strlen(dir) - 1] = '\0';
    } else if (!remote && strlen(dir) > root_len(dir)) {
        dir[strlen(dir) - 1] = '\0';
    }

    if (strpbrk(pattern, "*?") == NULL) {
        if (remote) {
            rc = remote_stat(path, &is_dir, &size, spelled);
            if (rc != REQ_OK) {
                say_failure(arg, rc);
                return 0;
            }
            /* The directory and name as spelled on the server. */
            strcpy(dir, spelled);
            name = shell_base(spelled);
            dir[name - spelled > 0 ? name - spelled - 1 : 0] = '\0';
        } else {
            if (fs_stat(path, &e) != FS_OK) {
                printf("%s: No such file or directory\n", arg);
                return 0;
            }
            is_dir = e.dir;
            size = e.size;
            name = pattern;
            strcpy(spelled, name);
        }
        if (is_dir) {
            printf("%s: Is a directory\n", arg);
            return 0;
        }
        return list_entry(mark, name, (uint8_t)strlen(name), 0, size) ? 1 : 0;
    }

    /* The directory must exist, then its files that match. */
    if (remote) {
        rc = remote_stat(dir, &is_dir, &size, dir);
    } else if (fs_stat(dir, &e) == FS_OK) {
        rc = REQ_OK;
        is_dir = e.dir;
    } else {
        rc = missing();
    }
    if (rc != REQ_OK) {
        say_failure(arg, rc);
        return 0;
    }
    if (!is_dir) {
        printf("%s: Not a directory\n", arg);
        return 0;
    }
    rc = list_dir(remote, dir, mark);
    if (rc != REQ_OK) {
        say_failure(arg, rc);
        return 0;
    }
    /* The whole directory stays listed; chosen() picks out the files. */
    at = list_top();
    while (list_next(mark, &at, &name, &len, &is_dir, &size)) {
        if (is_dir || len > SHELL_PATH_MAX) {
            continue;
        }
        memcpy(spelled, name, len);
        spelled[len] = '\0';
        if (shell_wildcard(pattern, spelled)) {
            count++;
        }
    }
    if (count == 0) {
        printf("%s: No match\n", arg);
    }
    return count;
}

/* Whether an expanded entry is one of the files: all of them for a single
 * file, otherwise the ones matching the pattern. */
static int chosen(const char *pattern, const char *name, uint8_t len, uint8_t dir)
{
    char buf[SHELL_PATH_MAX + 1];

    if (dir || len > SHELL_PATH_MAX) {
        return 0;
    }
    if (strpbrk(pattern, "*?") == NULL) {
        return 1;
    }
    memcpy(buf, name, len);
    buf[len] = '\0';
    return shell_wildcard(pattern, buf);
}

/* Announce a batch of several files, their names in columns. */
static void batch(const char *verb, const char *pattern, uint16_t mark, unsigned count)
{
    uint16_t at = list_top();
    const char *name;
    uint8_t len;
    uint8_t dir;
    uint32_t size;
    unsigned cell = 0;
    unsigned per_row;
    unsigned column = 0;
    unsigned pad = 0;

    if (count < 2) {
        return;
    }
    while (list_next(mark, &at, &name, &len, &dir, &size)) {
        if (chosen(pattern, name, len, dir) && len + 2u > cell) {
            cell = len + 2u;
        }
    }
    per_row = SCREEN_WIDTH / cell ? SCREEN_WIDTH / cell : 1;
    printf("%s %u files:\n", verb, count);
    at = list_top();
    while (list_next(mark, &at, &name, &len, &dir, &size)) {
        if (!chosen(pattern, name, len, dir)) {
            continue;
        }
        /* Pad the cell before, so that no row ends in spaces. */
        printf("%*s%.*s", (int)pad, "", len, name);
        pad = cell - len;
        if (++column == per_row) {
            printf("\n");
            column = 0;
            pad = 0;
        }
    }
    if (column != 0) {
        printf("\n");
    }
}

/* A name for the temporary file: NAME.$$$ beside the destination. */
static int temp_name(char *out, const char *dir, const char *name)
{
    const char *dot = strrchr(name, '.');
    size_t base = dot ? (size_t)(dot - name) : strlen(name);
    char alt[FS_NAME_MAX + 5];

    if (base + 4 >= sizeof alt) {
        return 0;
    }
    memcpy(alt, name, base);
    strcpy(alt + base, ".$$$");
    strcpy(out, dir);
    return join(out, alt, strlen(alt));
}

/* Copy one remote file here. Returns 1, or 0 after saying why not. */
static int receive(const char *rdir, const char *name, int force, const char *count)
{
    char path[SHELL_PATH_MAX + 1];
    char dest[FS_PATH_MAX + 1];
    char temp[FS_PATH_MAX + 1];
    char shown[SHELL_PATH_MAX + 1];
    struct fs_entry e;
    FILE *out;
    const uint8_t *r;
    uint16_t len;
    uint16_t chunk = (uint16_t)(req_limit() - 8);
    uint32_t offset = 0;
    uint32_t size = 0;
    uint32_t mtime = 0;
    uint8_t *p;
    int rc;
    int ok = 0;

    shell_ascii(name, shown, sizeof shown);
    if (strlen(name) > FS_NAME_MAX || !fs_name_ok(name)) {
        printf("%s%s: not a valid name here\n", count, shown);
        return 0;
    }
    strcpy(dest, lcwd);
    if (!join(dest, name, strlen(name)) || !temp_name(temp, lcwd, name)
        || !shell_target(rdir, name, path, sizeof path)) {
        printf("%s%s: the path is too long\n", count, shown);
        return 0;
    }
    if (!force && fs_stat(dest, &e) == FS_OK) {
        printf("%s%s: File exists; use get -f to replace it\n", count, shown);
        return 0;
    }
    out = fopen(temp, "wb");
    if (out == NULL) {
        printf("%s%s: cannot create %s (here)\n", count, shown, temp);
        return 0;
    }
    progress_begin(count, "Receiving", name);
    for (;;) {
        if (stop()) {
            printf("^C\nTransfer cancelled.\n");
            break;
        }
        p = req_payload();
        put32(p, offset);
        put16(p + 4, chunk);
        rc = req_call(MSG_FILE_READ, (uint16_t)(6 + put_path(p + 6, path)), &r, &len);
        if (rc != REQ_OK) {
            printf("\n");
            say_failure(name, rc);
            break;
        }
        if (len < 8 || len - 8 > chunk) {
            printf("\n");
            say_failure(name, REQ_BAD);
            break;
        }
        if (offset == 0) {
            size = get32(r);
            mtime = get32(r + 4);
        } else if (get32(r) != size || get32(r + 4) != mtime) {
            printf("\n%s: the file changed on the server during the transfer\n", shown);
            break;
        }
        if (fwrite(r + 8, 1, len - 8, out) != (size_t)(len - 8)) {
            printf("\n%s: cannot write it (here)\n", shown);
            break;
        }
        offset += len - 8;
        progress_update(offset, size);
        if (offset >= size) {
            if (offset > size) {
                printf("\n%s: the server sent more than the file's length\n", shown);
                break;
            }
            ok = 1;
            break;
        }
        if (len == 8) {
            printf("\n%s: the file ended early on the server\n", shown);
            break;
        }
    }
    if (fclose(out) != 0 && ok) {
        printf("\n%s: cannot write it (here)\n", shown);
        ok = 0;
    }
    if (ok && force && fs_stat(dest, &e) == FS_OK && remove(dest) != 0) {
        printf("\n%s: cannot replace it (here)\n", shown);
        ok = 0;
    }
    if (ok && rename(temp, dest) != 0) {
        printf("\n%s: cannot rename %s (here)\n", shown, temp);
        ok = 0;
    }
    if (!ok) {
        remove(temp);
        return 0;
    }
    if (mtime != 0) {
        fs_set_mtime(dest, mtime);
    }
    progress_done(size);
    return 1;
}

/* Copy one local file to the remote directory. Returns 1, or 0 after
 * saying why not. */
static int send_file(const char *ldir, const char *name, int force, const char *count)
{
    char src[FS_PATH_MAX + 1];
    char dest[SHELL_PATH_MAX + 1];
    char shown[SHELL_PATH_MAX + 1];
    struct fs_entry e;
    FILE *in;
    const uint8_t *r;
    uint16_t len;
    uint16_t chunk = (uint16_t)(req_limit() - 4);
    uint16_t want;
    uint32_t offset = 0;
    uint32_t crc = CRC32_INIT;
    size_t got;
    uint8_t *p;
    int rc;

    shell_ascii(name, shown, sizeof shown);
    strcpy(src, ldir);
    if (!join(src, name, strlen(name)) || !shell_target(rcwd, name, dest, sizeof dest)) {
        printf("%s%s: the path is too long\n", count, shown);
        return 0;
    }
    in = fopen(src, "rb");
    if (in == NULL || fs_stat(src, &e) != FS_OK) {
        if (in != NULL) {
            fclose(in);
        }
        printf("%s%s: cannot open it (here)\n", count, shown);
        return 0;
    }
    p = req_payload();
    put32(p, e.size);
    p[4] = force ? FLAG_REPLACE : 0;
    len = (uint16_t)(5 + put_path(p + 5, dest));
    put32(p + len, e.mtime);
    rc = req_call(MSG_FILE_BEGIN, (uint16_t)(len + 4), &r, &len);
    if (rc != REQ_OK) {
        fclose(in);
        printf("%s%s: %s%s\n", count, shown, failure(rc),
               rc == REQ_REFUSED && req_class == ERR_EXISTS ? "; use put -f to replace it" : "");
        return 0;
    }
    if (len > 0 && (r[0] & 3) == 2) {
        printf("Note: an earlier copy of %s was interrupted; the old version is in its .BAK file.\n",
               shown);
    }
    progress_begin(count, "Sending", name);
    while (offset < e.size) {
        if (stop()) {
            fclose(in);
            req_call(MSG_FILE_ABORT, 0, &r, &len);
            printf("^C\nTransfer cancelled.\n");
            return 0;
        }
        want = e.size - offset < chunk ? (uint16_t)(e.size - offset) : chunk;
        p = req_payload();
        put32(p, offset);
        got = fread(p + 4, 1, want, in);
        if (got == 0) {
            fclose(in);
            req_call(MSG_FILE_ABORT, 0, &r, &len);
            printf("\n%s: the file got shorter while it was being sent\n", shown);
            return 0;
        }
        crc = crc32_update(crc, p + 4, (uint16_t)got);
        rc = req_call(MSG_FILE_DATA, (uint16_t)(4 + got), &r, &len);
        if (rc != REQ_OK) {
            fclose(in);
            printf("\n%s%s: %s\n", count, shown, failure(rc));
            return 0;
        }
        offset += (uint32_t)got;
        progress_update(offset, e.size);
    }
    fclose(in);
    crc = ~crc;
    p = req_payload();
    put32(p, e.size);
    put32(p + 4, crc);
    rc = req_call(MSG_FILE_COMMIT, 8, &r, &len);
    if (rc != REQ_OK) {
        printf("\n%s%s: %s\n", count, shown, failure(rc));
        return 0;
    }
    if (len < 4 || get32(r) != crc) {
        printf("\n%s: the server's CRC-32 differs; it may not have the file\n", shown);
        return 0;
    }
    progress_done(e.size);
    return 1;
}

/* get [-f] FILE and put [-f] FILE. */
static void copy(int get, char *arg)
{
    char dir[FS_PATH_MAX + 1];
    char name[SHELL_PATH_MAX + 1];
    char count[24];
    const char *pattern;
    const char *entry;
    int force = shell_flag(&arg, "-f");
    uint16_t mark;
    uint16_t at;
    uint8_t len;
    uint8_t is_dir;
    uint32_t size;
    unsigned files;
    unsigned i = 0;
    int ok;

    if (arg[0] == '\0') {
        printf("Usage: %s [-f] FILE\n", get ? "get" : "put");
        return;
    }
    if (!get && !(server.capabilities & CAP_FILE_WRITE)) {
        printf("put: the server is read-only\n");
        return;
    }
    if (!get && force && !(server.capabilities & CAP_FILE_REPLACE)) {
        printf("put: the server does not replace files\n");
        return;
    }
    list_init();
    mark = list_mark();
    files = expand(get, arg, mark, dir);
    if (files == 0) {
        list_release(mark);
        return;
    }
    pattern = arg + strlen(arg);
    while (pattern > arg && !is_sep(pattern[-1]) && pattern[-1] != ':') {
        pattern--;
    }
    batch(get ? "Getting" : "Putting", pattern, mark, files);
    at = list_top();
    while (list_next(mark, &at, &entry, &len, &is_dir, &size)) {
        if (!chosen(pattern, entry, len, is_dir)) {
            continue;
        }
        memcpy(name, entry, len);
        name[len] = '\0';
        counter(count, i, files);
        ok = get ? receive(dir, name, force, count) : send_file(dir, name, force, count);
        if (!ok) {
            if (i + 1 < files && !lost) {
                printf("Remaining files not copied.\n");
            }
            break;
        }
        i++;
    }
    list_release(mark);
}

/*
 * The prompt.
 */

/* Read a line with Backspace, Esc (clear) and Ctrl-C (discard). Returns
 * 0 at Ctrl-D or Ctrl-Z on an empty line, or the end of input. */
static int read_line(char *line, size_t size)
{
    static int after_cr;
    size_t len = 0;
    int key;

    for (;;) {
        key = port_key();
        if (broken) {
            broken = 0;
            key = 3;
        }
        if (key == PORT_NO_KEY) {
            continue;
        }
        /* CR LF, from a file, ends one line. */
        if (key == '\n' && after_cr) {
            after_cr = 0;
            continue;
        }
        after_cr = key == '\r';
        if (key == PORT_EOF) {
            if (len == 0) {
                printf("\n");
                return 0;
            }
            line[len] = '\0';
            printf("\n");
            return 1;
        }
        if (key == '\r' || key == '\n') {
            line[len] = '\0';
            printf("\n");
            return 1;
        }
        if ((key == 4 || key == 26) && len == 0) {
            printf("\n");
            return 0;
        }
        if (key == 8 || key == 127) {
            if (len > 0) {
                len--;
                printf("\b \b");
            }
        } else if (key == 27) {
            while (len > 0) {
                len--;
                printf("\b \b");
            }
        } else if (key == 3) {
            printf("^C\n");
            line[0] = '\0';
            return 1;
        } else if (key >= 0x20 && key <= 0x7E && len + 1 < size) {
            line[len++] = (char)key;
            putchar(key);
        }
        fflush(stdout);
    }
}

static const char *platform_name(uint8_t platform)
{
    static char other[24];

    switch (platform) {
    case PLATFORM_DOS:
        return "DOS on x86";
    case 2:
        return "Apple II";
    case PLATFORM_LINUX:
        return "Linux";
    default:
        sprintf(other, "unknown platform %u", platform);
        return other;
    }
}

/* Run commands until the user quits or the link fails. */
static void shell(void)
{
    char line[LINE_MAX];
    char *command;
    char *arg;
    const char *name;
    int here;

    for (;;) {
        if (lost) {
            return;
        }
        printf("/%s> ", rcwd);
        fflush(stdout);
        if (!read_line(line, sizeof line)) {
            printf("Goodbye.\n");
            return;
        }
        shell_split(line, &command, &arg);
        if (command[0] == '\0') {
            continue;
        }
        name = shell_command(command, &here);
        if (name == NULL) {
            printf("Unknown command: %s\nType help for a list of commands.\n", command);
        } else if (!here) {
            printf("%s: not available in %s\n", command,
                   strcmp(name, "protocol") == 0 ? "native mode" : "this client");
        } else if (strcmp(name, "quit") == 0) {
            printf("Goodbye.\n");
            return;
        } else if (strcmp(name, "help") == 0) {
            shell_help(footer);
        } else if (strcmp(name, "pwd") == 0) {
            printf("/%s\n", rcwd);
        } else if (strcmp(name, "lpwd") == 0) {
            printf("%s\n", lcwd);
        } else if (strcmp(name, "cd") == 0) {
            cd(arg);
        } else if (strcmp(name, "lcd") == 0) {
            lcd(arg);
        } else if (strcmp(name, "ls") == 0 || strcmp(name, "dir") == 0) {
            ls(1, arg);
        } else if (strcmp(name, "lls") == 0 || strcmp(name, "ldir") == 0) {
            ls(0, arg);
        } else if (strcmp(name, "get") == 0) {
            copy(1, arg);
        } else if (strcmp(name, "put") == 0) {
            copy(0, arg);
        }
    }
}

int cli_run(const char *port, long baud, uint8_t *tx, uint8_t *rx, uint16_t max_payload,
            uint16_t wait)
{
    const uint8_t *r;
    uint16_t len;
    int rc;

    if (getcwd(home, sizeof home) == NULL) {
        strcpy(home, ".");
    }
    strcpy(lcwd, home);
    signal(SIGINT, on_break);
    if (port_open(port, baud) != 0) {
        return 1;
    }
    req_init(tx, rx, max_payload, baud, stop);
    if (wait == 0) {
        wait = HELLO_WAIT;
    }
    printf("Calling the server on %s at %ld; waiting up to %us. Press Esc to give up.\n", port,
           baud, wait);
    rc = req_hello(wait, &server);
    if (rc != REQ_OK) {
        if (rc == REQ_REFUSED && req_class == ERR_PROTOCOL_MISMATCH) {
            printf("The server speaks protocol %u.%u, not %u.\n", req_code & 0xFF, req_code >> 8,
                   PROTO_MAJOR);
        } else if (rc == REQ_LOST) {
            printf("No answer from the server.\n");
        } else {
            printf("Cannot start a session: %s.\n", failure(rc));
        }
        port_close();
        return 1;
    }
    printf("Connected to SPROCKET %s on %s, protocol %u.%u, %s.\n",
           server.version[0] ? server.version : "(no version)", platform_name(server.platform),
           PROTO_MAJOR, server.minor,
           server.capabilities & CAP_FILE_WRITE ? "writable" : "read-only");
    if (server.minor < 1 || !(server.capabilities & CAP_FILE_TREE)) {
        printf("This server does not share files (it needs protocol 1.1 and FILE_TREE).\n");
    } else {
        printf("Type help for a list of commands.\n");
        shell();
    }
    if (lost) {
        printf("Connection lost: no response from the server.\n");
        port_close();
        return 1;
    }
    req_call(MSG_CLOSE, 0, &r, &len);
    port_close();
    return 0;
}
