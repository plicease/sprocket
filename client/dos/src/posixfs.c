/* SPDX-License-Identifier: MIT */
/*
 * POSIX filesystem layer, so the DOS program's file code can be tested on
 * Linux. Times are converted to DOS dates in local time, as on DOS.
 */
#define _DEFAULT_SOURCE
#include <dirent.h>
#include <errno.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>

#include "fsys.h"

static uint32_t dos_time(time_t t)
{
    struct tm tm;

    if (localtime_r(&t, &tm) == NULL || tm.tm_year < 80 || tm.tm_year > 207) {
        return 0;
    }
    return ((uint32_t)(tm.tm_year - 80) << 25) | ((uint32_t)(tm.tm_mon + 1) << 21)
         | ((uint32_t)tm.tm_mday << 16) | ((uint32_t)tm.tm_hour << 11)
         | ((uint32_t)tm.tm_min << 5) | (uint32_t)(tm.tm_sec / 2);
}

static int usable(const char *name)
{
    const char *p;

    if (name[0] == '.' || strlen(name) > FS_NAME_MAX) {
        return 0;
    }
    for (p = name; *p; p++) {
        if (*p < 0x20 || *p > 0x7E) {
            return 0;
        }
    }
    return 1;
}

int fs_stat(const char *path, struct fs_entry *e)
{
    struct stat st;

    if (stat(path, &st) != 0) {
        return FS_ERROR;
    }
    if (!S_ISDIR(st.st_mode) && !S_ISREG(st.st_mode)) {
        errno = ENOENT;
        return FS_ERROR;
    }
    e->dir = (uint8_t)(S_ISDIR(st.st_mode) != 0);
    e->size = e->dir ? 0 : (uint32_t)st.st_size;
    e->mtime = dos_time(st.st_mtime);
    return FS_OK;
}

int fs_scan(const char *path, uint16_t index, struct fs_entry *e)
{
    char full[FS_PATH_MAX + FS_NAME_MAX + 2];
    DIR *d = opendir(path);
    struct dirent *item;
    int result = FS_END;

    if (d == NULL) {
        return FS_ERROR;
    }
    while ((item = readdir(d)) != NULL) {
        if (!usable(item->d_name) || strlen(path) + strlen(item->d_name) + 2 > sizeof full) {
            continue;
        }
        strcpy(full, path);
        strcat(full, "/");
        strcat(full, item->d_name);
        if (fs_stat(full, e) != FS_OK) {
            continue;
        }
        if (index == 0) {
            strcpy(e->name, item->d_name);
            result = FS_OK;
            break;
        }
        index--;
    }
    closedir(d);
    return result;
}

int fs_set_mtime(const char *path, uint32_t mtime)
{
    struct tm tm;
    struct timeval tv[2];
    time_t t;

    memset(&tm, 0, sizeof tm);
    tm.tm_year = (int)(mtime >> 25) + 80;
    tm.tm_mon = (int)((mtime >> 21) & 0xF) - 1;
    tm.tm_mday = (int)((mtime >> 16) & 0x1F);
    tm.tm_hour = (int)((mtime >> 11) & 0x1F);
    tm.tm_min = (int)((mtime >> 5) & 0x3F);
    tm.tm_sec = (int)(mtime & 0x1F) * 2;
    tm.tm_isdst = -1;
    t = mktime(&tm);
    if (t == (time_t)-1) {
        return FS_ERROR;
    }
    tv[0].tv_sec = t;
    tv[0].tv_usec = 0;
    tv[1] = tv[0];
    return utimes(path, tv) == 0 ? FS_OK : FS_ERROR;
}

int fs_name_ok(const char *name)
{
    return name[0] != '\0' && strlen(name) <= FS_NAME_MAX;
}

uint16_t fs_error_code(void)
{
    return (uint16_t)errno;
}
