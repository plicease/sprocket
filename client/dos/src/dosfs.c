/* SPDX-License-Identifier: MIT */
/*
 * DOS filesystem layer: directory scans with find-first/find-next, which
 * also give each entry's size, date and time in DOS's own format.
 */
#include <dos.h>
#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <stdlib.h>
#include <string.h>

#include "fsys.h"

#define SKIP (_A_HIDDEN | _A_SYSTEM | _A_VOLID)

static int usable(const struct find_t *f)
{
    const char *p;

    if (f->attrib & SKIP || f->name[0] == '.') {
        return 0;
    }
    for (p = f->name; *p; p++) {
        if (*p < 0x20 || *p > 0x7E) {
            return 0;
        }
    }
    return strlen(f->name) <= FS_NAME_MAX;
}

static void fill(struct fs_entry *e, const struct find_t *f)
{
    e->dir = (uint8_t)((f->attrib & _A_SUBDIR) != 0);
    e->size = e->dir ? 0 : f->size;
    e->mtime = ((uint32_t)f->wr_date << 16) | f->wr_time;
}

int fs_scan(const char *path, uint16_t index, struct fs_entry *e)
{
    static char pattern[FS_PATH_MAX + 5];
    struct find_t f;
    size_t len = strlen(path);
    unsigned rc;

    if (len + 5 > sizeof pattern) {
        errno = ENOENT;
        return FS_ERROR;
    }
    memcpy(pattern, path, len);
    if (len > 0 && path[len - 1] != '\\' && path[len - 1] != ':') {
        pattern[len++] = '\\';
    }
    strcpy(pattern + len, "*.*");
    rc = _dos_findfirst(pattern, _A_SUBDIR | _A_RDONLY | _A_ARCH | SKIP, &f);
    while (rc == 0) {
        if (usable(&f)) {
            if (index == 0) {
                fill(e, &f);
                strcpy(e->name, f.name);
                return FS_OK;
            }
            index--;
        }
        rc = _dos_findnext(&f);
    }
    /*
     * "No more files", or "file not found" from find-first in an empty
     * root directory, is the end; "path not found" means no directory.
     */
    if (rc == 0x12 || rc == 0x02) {
        return FS_END;
    }
    errno = ENOENT;
    return FS_ERROR;
}

int fs_stat(const char *path, struct fs_entry *e)
{
    struct find_t f;
    size_t len = strlen(path);

    /* The root of a drive has no directory entry of its own. */
    if ((len == 3 && path[1] == ':' && path[2] == '\\') || (len == 1 && path[0] == '\\')
        || (len == 2 && path[1] == ':')) {
        e->dir = 1;
        e->size = 0;
        e->mtime = 0;
        return FS_OK;
    }
    if (_dos_findfirst(path, _A_SUBDIR | _A_RDONLY | _A_ARCH | SKIP, &f) != 0) {
        errno = ENOENT;
        return FS_ERROR;
    }
    fill(e, &f);
    return FS_OK;
}

int fs_set_mtime(const char *path, uint32_t mtime)
{
    int fd;
    unsigned rc;

    if (_dos_open(path, O_RDWR, &fd) != 0) {
        return FS_ERROR;
    }
    rc = _dos_setftime(fd, (unsigned)(mtime >> 16), (unsigned)(mtime & 0xFFFF));
    _dos_close(fd);
    return rc == 0 ? FS_OK : FS_ERROR;
}

int fs_name_ok(const char *name)
{
    static const char *const devices[] = {
        "CON", "AUX", "PRN", "NUL", "CLOCK$", "COM1", "COM2", "COM3", "COM4",
        "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4",
        "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"
    };
    const char *dot = strchr(name, '.');
    size_t base = dot ? (size_t)(dot - name) : strlen(name);
    const char *p;
    unsigned i;

    if (base == 0 || base > 8 || (dot && (strlen(dot + 1) > 3 || strchr(dot + 1, '.')))) {
        return 0;
    }
    for (p = name; *p; p++) {
        if (*p <= ' ' || *p > 0x7E || strchr("\"*+,/:;<=>?[\\]|", *p)) {
            return 0;
        }
    }
    for (i = 0; i < sizeof devices / sizeof devices[0]; i++) {
        if (strlen(devices[i]) == base && strnicmp(name, devices[i], base) == 0) {
            return 0;
        }
    }
    return 1;
}

uint16_t fs_error_code(void)
{
    return (uint16_t)_doserrno;
}
