/* SPDX-License-Identifier: MIT */
/*
 * The command shell's portable parts, matching the Linux client
 * (src/shell.rs): the command vocabulary and its help, sizes, listings,
 * wildcards, and typed tree paths. No I/O besides printing.
 */
#ifndef SPROCKET_SHELL_H
#define SPROCKET_SHELL_H

#include <stddef.h>
#include <stdint.h>

/* Longest tree path (protocol v1.1), and so the longest listing name. */
#define SHELL_PATH_MAX 127
/* Names longer than this push their size out of the listing's column. */
#define SHELL_NAME_COLUMN 40

/*
 * The command's canonical name for a typed name or alias, ignoring case,
 * or NULL if no client knows it. *available is set to whether this client
 * can run it.
 */
const char *shell_command(const char *name, int *available);

/* Print the help: the commands this client runs, then footer, then the
 * ones it does not. */
void shell_help(const char *footer);

/* Split a line in place into a command and its argument, both trimmed. */
void shell_split(char *line, char **command, char **arg);

/* Take a leading flag such as -R off *arg; returns whether it was there. */
int shell_flag(char **arg, const char *flag);

/* A size such as 512B, 1.5kB or 12MB, as tests/shell/human.txt shows.
 * out must hold 12 bytes. */
void shell_human(uint32_t size, char *out);

/* Match name against pattern with * and ?, ignoring ASCII case. */
int shell_wildcard(const char *pattern, const char *name);

/* Compare names for listing order: ignoring ASCII case, then exactly. */
int shell_order(const char *a, const char *b);

/*
 * Resolve a typed path against cwd (both tree paths without a leading /,
 * components joined by /) into out, of size bytes. A leading / starts at
 * the top, and .. stops there. Returns 0 if the result does not fit.
 */
int shell_target(const char *cwd, const char *arg, char *out, size_t size);

/* The last component of a tree path. */
const char *shell_base(const char *path);

/* Copy a name with anything but printable ASCII shown as ?. */
void shell_ascii(const char *name, char *out, size_t size);

/*
 * Listings, kept in one arena: the lines to print grow from the bottom,
 * and each directory being read is collected, sorted, at the top.
 */
void list_init(void);

/* Add a line: a file with its size, or a directory (name ending in /).
 * Returns 0 if the arena is full. */
int list_line(const char *name, uint8_t dir, uint32_t size);

/* Print the lines with sizes in a column, then clear them. Stops early
 * and returns 0 if stop() says so. */
int list_print(int (*stop)(void));

/*
 * A directory's entries, collected in order at the top of the arena:
 * take a mark, add entries (0 if the arena is full), then step through
 * them from list_top() until list_next returns 0. Entries collected for a
 * subdirectory while stepping go below, so they do not disturb these.
 * list_release(mark) drops everything collected since the mark.
 */
uint16_t list_mark(void);
int list_entry(uint16_t mark, const char *name, uint8_t len, uint8_t dir, uint32_t size);
uint16_t list_top(void);
int list_next(uint16_t mark, uint16_t *at, const char **name, uint8_t *len, uint8_t *dir,
              uint32_t *size);
void list_release(uint16_t mark);

#endif
