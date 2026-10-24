/* SPDX-License-Identifier: MIT */
/* Putting a received file in place (place.h). */
#include <stdio.h>
#include <string.h>

#include "fsys.h"
#include "place.h"

/* Length of the directory part of path, with its trailing separator. */
static size_t dir_len(const char *path)
{
    size_t len = strlen(path);

    while (len > 0 && path[len - 1] != '/' && path[len - 1] != '\\' && path[len - 1] != ':') {
        len--;
    }
    return len;
}

int place_free_name(char *out, const char *dest, const char *name, char mark)
{
    const char *dot = strrchr(name, '.');
    size_t base = dot ? (size_t)(dot - name) : strlen(name);
    size_t dir = dir_len(dest);
    struct fs_entry e;
    char ext[5];
    int i;

    if (dir + base + 4 > FS_PATH_MAX) {
        return 0;
    }
    memcpy(out, dest, dir);
    memcpy(out + dir, name, base);
    for (i = -1; i < 10; i++) {
        ext[0] = '.';
        ext[1] = '$';
        ext[2] = mark;
        ext[3] = (char)(i < 0 ? '$' : '0' + i);
        ext[4] = '\0';
        strcpy(out + dir + base, ext);
        if (fs_stat(out, &e) != FS_OK) {
            return 1;
        }
    }
    return 0;
}

int place_file(const char *temp, const char *dest)
{
    char backup[FS_PATH_MAX + 1];
    struct fs_entry e;
    int replacing = fs_stat(dest, &e) == FS_OK;

    if (replacing) {
        if (!place_free_name(backup, dest, dest + dir_len(dest), 'B')
            || rename(dest, backup) != 0) {
            return 0;
        }
    }
    if (rename(temp, dest) != 0) {
        if (replacing) {
            rename(backup, dest);
        }
        return 0;
    }
    if (replacing) {
        remove(backup);
    }
    return 1;
}
