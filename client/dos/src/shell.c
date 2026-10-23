/* SPDX-License-Identifier: MIT */
/*
 * The command shell's portable parts (shell.h), matching src/shell.rs on
 * the Linux side and the shared fixtures in tests/shell/.
 */
#include <stdio.h>
#include <string.h>

#include "shell.h"

/* Help lines are the usage padded to column 17, then the description;
 * continuation lines start with spaces. */
#define CONT "\n                   "

struct command {
    const char *names; /* the name, then aliases, separated by spaces */
    const char *help;  /* NULL if native mode has no such command */
    uint8_t here;      /* this client runs it */
};

/* Every command either interface knows, in alphabetical order. */
static const struct command vocabulary[] = {
    { "cd", "cd [DIR]         change the remote directory; with no DIR, go to the top", 1 },
    { "dir", "dir [-R] [PATH]  same as ls", 1 },
    { "get", "get [-f] FILE    copy remote files here; FILE may use * and ?" CONT
             "wildcards; -f replaces existing files", 1 },
    { "help ?", "help             show this list", 1 },
    { "lcd", "lcd [DIR]        change the local directory; with no DIR, go home", 1 },
    { "ldir", "ldir [PATH]      same as lls", 1 },
    { "lls", "lls [PATH]       list a local directory", 1 },
    { "lpwd", "lpwd             show the local directory", 1 },
    { "ls", "ls [-R] [PATH]   list a remote directory; -R lists everything below it", 1 },
    { "protocol", NULL, 0 },
    { "put", "put [-f] FILE    copy local files to the remote directory; FILE may" CONT
             "use * and ? wildcards; -f replaces existing files", 1 },
    { "pwd", "pwd              show the remote directory", 1 },
    { "quit bye exit", "quit             end the session (or Ctrl-D)", 1 },
    { "set", "set editing on|off" CONT "turn line editing on or off for this session", 0 },
};

#define COMMANDS (sizeof vocabulary / sizeof vocabulary[0])

static int lower(int ch)
{
    return ch >= 'A' && ch <= 'Z' ? ch + ('a' - 'A') : ch;
}

/* The canonical name of a command: its first name. */
static void first_name(const struct command *c, char *out)
{
    size_t n = strcspn(c->names, " ");

    memcpy(out, c->names, n);
    out[n] = '\0';
}

const char *shell_command(const char *name, int *available)
{
    static char found[16];
    unsigned i;
    const char *p;
    size_t len = strlen(name);
    size_t n;
    size_t k;

    for (i = 0; i < COMMANDS; i++) {
        for (p = vocabulary[i].names; *p; p += n + (p[n] == ' ')) {
            n = strcspn(p, " ");
            if (n != len) {
                continue;
            }
            for (k = 0; k < n && lower(name[k]) == p[k]; k++) {
            }
            if (k == n) {
                first_name(&vocabulary[i], found);
                *available = vocabulary[i].here;
                return found;
            }
        }
    }
    return NULL;
}

/* Print the names of the commands for which pick() holds, as a list. */
static void names(const char *intro, int (*pick)(const struct command *))
{
    char name[16];
    unsigned i;
    int count = 0;

    for (i = 0; i < COMMANDS; i++) {
        if (pick(&vocabulary[i])) {
            first_name(&vocabulary[i], name);
            printf("%s%s", count++ ? ", " : intro, name);
        }
    }
    if (count) {
        printf(".\n");
    }
}

static int not_native(const struct command *c)
{
    return c->help == NULL;
}

static int not_here(const struct command *c)
{
    return c->help != NULL && !c->here;
}

void shell_help(const char *footer)
{
    unsigned i;

    printf("Commands:\n");
    for (i = 0; i < COMMANDS; i++) {
        if (vocabulary[i].help != NULL && vocabulary[i].here) {
            printf("  %s\n", vocabulary[i].help);
        }
    }
    printf("%s", footer);
    names("Not available in native mode: ", not_native);
    names("Not available in this client: ", not_here);
}

static char *trim(char *s)
{
    char *end;

    while (*s == ' ' || *s == '\t') {
        s++;
    }
    end = s + strlen(s);
    while (end > s && (end[-1] == ' ' || end[-1] == '\t' || end[-1] == '\r' || end[-1] == '\n')) {
        *--end = '\0';
    }
    return s;
}

void shell_split(char *line, char **command, char **arg)
{
    char *space;

    line = trim(line);
    space = strchr(line, ' ');
    *command = line;
    if (space == NULL) {
        *arg = line + strlen(line);
        return;
    }
    *space = '\0';
    *arg = trim(space + 1);
}

int shell_flag(char **arg, const char *flag)
{
    size_t n = strlen(flag);

    if (strncmp(*arg, flag, n) != 0 || ((*arg)[n] != '\0' && (*arg)[n] != ' ')) {
        return 0;
    }
    *arg = trim(*arg + n);
    return 1;
}

static unsigned long long round_half_even(unsigned long long n, unsigned long long d)
{
    unsigned long long q = n / d;
    unsigned long long r = n % d;

    if (2 * r > d || (2 * r == d && q % 2 == 1)) {
        q++;
    }
    return q;
}

void shell_human(uint32_t size, char *out)
{
    static const char *const units[] = { "B", "kB", "MB", "GB", "TB" };
    unsigned long long n = size;
    unsigned long long divisor = 1;
    unsigned long long tenths;
    unsigned unit = 0;

    while (n >= 1024 * divisor && unit < 4) {
        divisor *= 1024;
        unit++;
    }
    /* A value that would round to 1024 is shown in the next unit up. */
    if (unit > 0 && 2 * n >= 2047 * divisor && unit < 4) {
        divisor *= 1024;
        unit++;
    }
    if (unit > 0 && 20 * n < 199 * divisor) {
        tenths = round_half_even(10 * n, divisor);
        if (tenths % 10 != 0) {
            sprintf(out, "%u.%u%s", (unsigned)(tenths / 10), (unsigned)(tenths % 10), units[unit]);
        } else {
            sprintf(out, "%u%s", (unsigned)(tenths / 10), units[unit]);
        }
        return;
    }
    sprintf(out, "%lu%s", (unsigned long)round_half_even(n, divisor), units[unit]);
}

int shell_wildcard(const char *pattern, const char *name)
{
    const char *star_p = NULL;
    const char *star_n = NULL;

    while (*name) {
        if (*pattern == '*') {
            star_p = ++pattern;
            star_n = name;
        } else if (*pattern != '\0' && (*pattern == '?' || lower(*pattern) == lower(*name))) {
            pattern++;
            name++;
        } else if (star_p != NULL) {
            pattern = star_p;
            name = ++star_n;
        } else {
            return 0;
        }
    }
    while (*pattern == '*') {
        pattern++;
    }
    return *pattern == '\0';
}

/* Compare two names of the given lengths, as shell_order. */
static int order(const char *a, size_t alen, const char *b, size_t blen)
{
    size_t i;
    size_t n = alen < blen ? alen : blen;
    int d;

    for (i = 0; i < n; i++) {
        d = lower((unsigned char)a[i]) - lower((unsigned char)b[i]);
        if (d != 0) {
            return d;
        }
    }
    if (alen != blen) {
        return alen < blen ? -1 : 1;
    }
    for (i = 0; i < n; i++) {
        if (a[i] != b[i]) {
            return (unsigned char)a[i] - (unsigned char)b[i];
        }
    }
    return 0;
}

int shell_order(const char *a, const char *b)
{
    return order(a, strlen(a), b, strlen(b));
}

int shell_target(const char *cwd, const char *arg, char *out, size_t size)
{
    size_t len = 0;
    size_t n;
    const char *p;
    int pass;

    out[0] = '\0';
    /* The current directory first, unless the path starts at the top. */
    for (pass = arg[0] == '/' ? 1 : 0; pass < 2; pass++) {
        for (p = pass ? arg : cwd; *p; p += n + (p[n] == '/')) {
            n = strcspn(p, "/");
            if (n == 0 || (n == 1 && p[0] == '.')) {
                continue;
            }
            if (n == 2 && p[0] == '.' && p[1] == '.') {
                while (len > 0 && out[len - 1] != '/') {
                    len--;
                }
                if (len > 0) {
                    len--;
                }
                out[len] = '\0';
                continue;
            }
            if (len + (len > 0) + n + 1 > size) {
                return 0;
            }
            if (len > 0) {
                out[len++] = '/';
            }
            memcpy(out + len, p, n);
            len += n;
            out[len] = '\0';
        }
    }
    return 1;
}

const char *shell_base(const char *path)
{
    const char *slash = strrchr(path, '/');

    return slash ? slash + 1 : path;
}

void shell_ascii(const char *name, char *out, size_t size)
{
    size_t i;

    for (i = 0; name[i] != '\0' && i + 1 < size; i++) {
        out[i] = name[i] >= 0x20 && name[i] <= 0x7E ? name[i] : '?';
    }
    out[i] = '\0';
}

/*
 * The listing arena. Each record is size (u32), dir (u8), name length
 * (u8), then the name. Lines are at [0, used); a directory being read is
 * at [top, its mark), in order.
 */
#ifdef __WATCOMC__
#define ARENA 12288
#else
#define ARENA 65000
#endif
#define RECORD 6

static uint8_t arena[ARENA];
static uint16_t used;
static uint16_t top = ARENA;

static void put_record(uint8_t *p, const char *name, uint8_t len, uint8_t dir, uint32_t size)
{
    memcpy(p, &size, 4);
    p[4] = dir;
    p[5] = len;
    memcpy(p + RECORD, name, len);
}

void list_init(void)
{
    used = 0;
    top = ARENA;
}

int list_line(const char *name, uint8_t dir, uint32_t size)
{
    size_t len = strlen(name);
    uint8_t *p;
    size_t i;

    if (len > 255 || used + RECORD + len > top) {
        return 0;
    }
    p = arena + used;
    put_record(p, name, (uint8_t)len, dir, size);
    for (i = 0; i < len; i++) {
        if (p[RECORD + i] < 0x20 || p[RECORD + i] > 0x7E) {
            p[RECORD + i] = '?';
        }
    }
    used = (uint16_t)(used + RECORD + len);
    return 1;
}

int list_print(int (*stop)(void))
{
    uint16_t at;
    uint8_t *p;
    unsigned width = 0;
    uint32_t size;
    char text[12];

    for (at = 0; at < used; at = (uint16_t)(at + RECORD + p[5])) {
        p = arena + at;
        if (!p[4] && p[5] > width) {
            width = p[5];
        }
    }
    if (width > SHELL_NAME_COLUMN) {
        width = SHELL_NAME_COLUMN;
    }
    for (at = 0; at < used; at = (uint16_t)(at + RECORD + p[5])) {
        p = arena + at;
        if (stop != NULL && stop()) {
            used = 0;
            return 0;
        }
        if (p[4]) {
            printf("%.*s\n", p[5], (const char *)p + RECORD);
        } else {
            memcpy(&size, p, 4);
            shell_human(size, text);
            printf("%-*.*s  %6s\n", (int)width, p[5], (const char *)p + RECORD, text);
        }
    }
    used = 0;
    return 1;
}

uint16_t list_mark(void)
{
    return top;
}

uint16_t list_top(void)
{
    return top;
}

int list_entry(uint16_t mark, const char *name, uint8_t len, uint8_t dir, uint32_t size)
{
    uint16_t need = (uint16_t)(RECORD + len);
    uint16_t at;
    uint8_t *p;

    if (top < used + need) {
        return 0;
    }
    /* Find the first entry that sorts after this one; it goes before. */
    for (at = top; at < mark; at = (uint16_t)(at + RECORD + p[5])) {
        p = arena + at;
        if (order((const char *)p + RECORD, p[5], name, len) > 0) {
            break;
        }
    }
    memmove(arena + top - need, arena + top, at - top);
    top = (uint16_t)(top - need);
    put_record(arena + at - need, name, len, dir, size);
    return 1;
}

int list_next(uint16_t mark, uint16_t *at, const char **name, uint8_t *len, uint8_t *dir,
              uint32_t *size)
{
    uint8_t *p = arena + *at;

    if (*at >= mark) {
        return 0;
    }
    memcpy(size, p, 4);
    *dir = p[4];
    *len = p[5];
    *name = (const char *)p + RECORD;
    *at = (uint16_t)(*at + RECORD + p[5]);
    return 1;
}

void list_release(uint16_t mark)
{
    top = mark;
}
