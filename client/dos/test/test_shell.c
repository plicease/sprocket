/* SPDX-License-Identifier: MIT */
/*
 * Runs the shell's portable parts (src/shell.c) against the fixtures the
 * Linux client also uses: tests/shell/human.txt, listing.txt and
 * help-native.txt. Plus wildcards, typed paths and the listing arena.
 *
 * Usage: test_shell SHELL_FIXTURE_DIR
 */
#define _DEFAULT_SOURCE
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "../src/shell.h"
#include "vectors.h"

static const char *fixtures;
static char captured[8192];
static int saved_stdout = -1;
static char capture_path[] = "/tmp/test_shell.XXXXXX";
static int capture_fd = -1;

static void capture_begin(void)
{
    fflush(stdout);
    if (capture_fd < 0) {
        capture_fd = mkstemp(capture_path);
        unlink(capture_path);
    }
    if (ftruncate(capture_fd, 0) != 0 || lseek(capture_fd, 0, SEEK_SET) != 0) {
        exit(2);
    }
    saved_stdout = dup(1);
    dup2(capture_fd, 1);
}

static const char *capture_end(void)
{
    ssize_t n;

    fflush(stdout);
    dup2(saved_stdout, 1);
    close(saved_stdout);
    lseek(capture_fd, 0, SEEK_SET);
    n = read(capture_fd, captured, sizeof captured - 1);
    captured[n < 0 ? 0 : n] = '\0';
    return captured;
}

/* The whole of a fixture file. */
static char *slurp(const char *name)
{
    char path[512];
    FILE *f;
    long len;
    char *text;

    snprintf(path, sizeof path, "%s/%s", fixtures, name);
    f = fopen(path, "rb");
    if (f == NULL) {
        printf("cannot open %s\n", path);
        exit(2);
    }
    fseek(f, 0, SEEK_END);
    len = ftell(f);
    fseek(f, 0, SEEK_SET);
    text = malloc((size_t)len + 1);
    if (text == NULL || fread(text, 1, (size_t)len, f) != (size_t)len) {
        exit(2);
    }
    text[len] = '\0';
    fclose(f);
    return text;
}

static void human(void)
{
    char *text = slurp("human.txt");
    char *line;
    char got[12];
    unsigned long long size;
    char *space;
    int cases = 0;

    for (line = strtok(text, "\n"); line != NULL; line = strtok(NULL, "\n")) {
        if (line[0] == '#' || line[0] == '\0') {
            continue;
        }
        space = strchr(line, ' ');
        size = strtoull(line, NULL, 10);
        if (size > 0xFFFFFFFFULL) {
            continue; /* the protocol's sizes are 32-bit */
        }
        shell_human((uint32_t)size, got);
        CHECK(strcmp(got, space + 1) == 0, "human(%llu) = %s, expected %s", size, got, space + 1);
        cases++;
    }
    CHECK(cases > 10, "human.txt cases");
    free(text);
}

static void listing(void)
{
    char *text = slurp("listing.txt");
    char *p = strstr(text, "\ncase: ");
    char *eol;
    char *expected;
    char *end;
    char name[64];
    char *rest;
    int cases = 0;

    while (p != NULL) {
        p += 7;
        eol = strchr(p, '\n');
        snprintf(name, sizeof name, "%.*s", (int)(eol - p), p);
        p = eol + 1;
        list_init();
        while (strncmp(p, "=\n", 2) != 0) {
            eol = strchr(p, '\n');
            *eol = '\0';
            if (p[0] == 'd') {
                list_line(p + 2, 1, 0);
            } else {
                list_line(strchr(p + 2, ' ') + 1, 0, (uint32_t)strtoul(p + 2, &rest, 10));
            }
            p = eol + 1;
        }
        expected = p + 2;
        end = strstr(expected, "end\n");
        *end = '\0';
        capture_begin();
        list_print(NULL);
        CHECK(strcmp(capture_end(), expected) == 0, "listing case %s:\n%s", name, captured);
        cases++;
        p = strstr(end + 1, "\ncase: ");
    }
    CHECK(cases >= 4, "listing.txt cases");
    free(text);
}

/* The fixture's help without the commands this client lacks, which it
 * names after the footer instead. */
static void help(void)
{
    char *text = slurp("help-native.txt");
    static char expected[4096];
    char *line;
    int skip = 0;

    expected[0] = '\0';
    for (line = strtok(text, "\n"); line != NULL; line = strtok(NULL, "\n")) {
        if (line[0] == '#') {
            continue;
        }
        if (strncmp(line, "  set ", 6) == 0) {
            skip = 1;
            continue;
        }
        if (skip && strncmp(line, "    ", 4) == 0) {
            continue;
        }
        skip = 0;
        strcat(expected, line);
        strcat(expected, "\n");
    }
    strcat(expected, "Not available in this client: set.\n");
    capture_begin();
    shell_help("");
    CHECK(strcmp(capture_end(), expected) == 0, "help:\n%s", captured);
    free(text);
}

static void words(void)
{
    char line[64];
    char *command;
    char *arg;
    int here;

    strcpy(line, "  GET   -f  *.EXE  ");
    shell_split(line, &command, &arg);
    CHECK(strcmp(command, "GET") == 0 && strcmp(arg, "-f  *.EXE") == 0, "split");
    CHECK(shell_flag(&arg, "-f") && strcmp(arg, "*.EXE") == 0, "flag taken off");
    CHECK(!shell_flag(&arg, "-f"), "no flag");
    arg = "-Rx";
    CHECK(!shell_flag(&arg, "-R"), "-Rx is not -R");
    strcpy(line, "ls");
    shell_split(line, &command, &arg);
    CHECK(strcmp(command, "ls") == 0 && arg[0] == '\0', "split without argument");

    CHECK(strcmp(shell_command("BYE", &here), "quit") == 0 && here, "alias");
    CHECK(strcmp(shell_command("?", &here), "help") == 0, "?");
    CHECK(strcmp(shell_command("protocol", &here), "protocol") == 0 && !here, "terminal only");
    CHECK(strcmp(shell_command("set", &here), "set") == 0 && !here, "set is not here");
    CHECK(shell_command("frob", &here) == NULL, "unknown");
}

static void wildcards(void)
{
    CHECK(shell_wildcard("*.exe", "DOOM.EXE"), "*.exe");
    CHECK(shell_wildcard("d??m.*", "doom.exe"), "?");
    CHECK(!shell_wildcard("*.exe", "doom.exe.txt"), "suffix");
    CHECK(shell_wildcard("*a*b*", "xxaxxbxx"), "several stars");
    CHECK(!shell_wildcard("a?", "a"), "? needs a character");
    CHECK(shell_wildcard("*", ""), "* matches nothing");
}

static void targets(void)
{
    char out[SHELL_PATH_MAX + 1];

    CHECK(shell_target("a/b", "", out, sizeof out) && strcmp(out, "a/b") == 0, "empty");
    CHECK(shell_target("a/b", "c/./d/", out, sizeof out) && strcmp(out, "a/b/c/d") == 0, "./");
    CHECK(shell_target("a/b", "..", out, sizeof out) && strcmp(out, "a") == 0, "..");
    CHECK(shell_target("a/b", "../../../..", out, sizeof out) && out[0] == '\0', ".. at the top");
    CHECK(shell_target("a/b", "/x", out, sizeof out) && strcmp(out, "x") == 0, "/x");
    CHECK(!shell_target("abc", "defgh", out, 8), "too long");
    CHECK(strcmp(shell_base("a/b/c.txt"), "c.txt") == 0 && strcmp(shell_base("c"), "c") == 0,
          "base");
}

/* Entries at two levels of the arena keep their own order. */
static void arena(void)
{
    uint16_t outer;
    uint16_t inner;
    uint16_t at;
    const char *name;
    uint8_t len;
    uint8_t dir;
    uint32_t size;
    char seen[64] = "";

    list_init();
    outer = list_mark();
    list_entry(outer, "b", 1, 0, 2);
    list_entry(outer, "A", 1, 0, 1);
    list_entry(outer, "c", 1, 1, 0);
    list_entry(outer, "a", 1, 0, 1);
    at = list_top();
    while (list_next(outer, &at, &name, &len, &dir, &size)) {
        strncat(seen, name, len);
        if (dir) {
            inner = list_mark();
            list_entry(inner, "z", 1, 0, 0);
            list_entry(inner, "y", 1, 0, 0);
            {
                uint16_t in_at = list_top();
                strcat(seen, "(");
                while (list_next(inner, &in_at, &name, &len, &dir, &size)) {
                    strncat(seen, name, len);
                }
                strcat(seen, ")");
            }
            list_release(inner);
        }
    }
    list_release(outer);
    CHECK(strcmp(seen, "Aabc(yz)") == 0, "arena order: %s", seen);
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        printf("usage: test_shell SHELL_FIXTURE_DIR\n");
        return 2;
    }
    fixtures = argv[1];
    human();
    listing();
    help();
    words();
    wildcards();
    targets();
    arena();
    printf("test_shell: %d failures\n", failures);
    return failures != 0;
}
