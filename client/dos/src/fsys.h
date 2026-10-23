/* SPDX-License-Identifier: MIT */
/*
 * The platform's filesystem, as far as the file tree needs it: directory
 * scans, an entry's size and time, and setting a file's time. Reading and
 * writing use C stdio, which is the same everywhere. dosfs.c uses DOS
 * calls; posixfs.c lets the same code run and be tested on Linux.
 *
 * Paths here are native paths (C:\SHARE\GAMES on DOS). Errors set errno.
 */
#ifndef SPROCKET_FSYS_H
#define SPROCKET_FSYS_H

#include <stdint.h>

#ifdef __WATCOMC__
#define FS_SEP '\\'
/* Longest name: 8.3. */
#define FS_NAME_MAX 12
#else
#define FS_SEP '/'
#define FS_NAME_MAX 64
#endif

/* Longest native path built from a root and a tree path. */
#define FS_PATH_MAX 200

struct fs_entry {
    uint8_t dir;
    uint32_t size;
    uint32_t mtime; /* DOS date and time; 0 if unknown */
    char name[FS_NAME_MAX + 1];
};

/* Results of fs_scan. */
#define FS_OK 0
#define FS_END 1
#define FS_ERROR (-1)

/*
 * The index-th entry of the directory at path, counting from 0 in the
 * filesystem's own order. Hidden and system entries, "." and "..", names
 * starting with ".", longer than FS_NAME_MAX or outside printable ASCII,
 * and anything but files and directories are skipped and not counted.
 * Returns FS_OK, FS_END after the last entry, or FS_ERROR.
 */
int fs_scan(const char *path, uint16_t index, struct fs_entry *e);

/* Kind, size and time of the entry at path; e->name is not set. Returns
 * FS_OK or FS_ERROR. */
int fs_stat(const char *path, struct fs_entry *e);

/* Give the file at path a DOS date and time. Returns FS_OK or FS_ERROR. */
int fs_set_mtime(const char *path, uint32_t mtime);

/* Nonzero if this filesystem can create a file with this name: on DOS an
 * 8.3 name that is not a device. */
int fs_name_ok(const char *name);

/* The platform's code for the last error, for ERROR responses. */
uint16_t fs_error_code(void);

#endif
