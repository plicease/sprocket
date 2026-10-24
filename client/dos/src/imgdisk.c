/* SPDX-License-Identifier: MIT */
/*
 * Disks for testing the program on Linux: the image files named in the
 * environment variable SPROCKET_DISKS, separated by colons. An image the
 * size of a standard floppy is a removable disk with that geometry;
 * anything else a fixed disk of 16 heads and 63 sectors a track, or 4
 * heads and 17, whichever divides its size.
 */
#define _DEFAULT_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "disk.h"

static char paths[DISK_MAX][256];

static const struct {
    long size;
    uint16_t cylinders;
    uint8_t heads;
    uint8_t sectors;
    const char *name;
} floppies[] = {
    { 163840L, 40, 1, 8, "160K" },   { 184320L, 40, 1, 9, "180K" },
    { 327680L, 40, 2, 8, "320K" },   { 368640L, 40, 2, 9, "360K" },
    { 737280L, 80, 2, 9, "720K" },   { 1228800L, 80, 2, 15, "1.2M" },
    { 1474560L, 80, 2, 18, "1.44M" }, { 2949120L, 80, 2, 36, "2.88M" },
};

static int geometry(struct disk *d, long size)
{
    unsigned i;

    for (i = 0; i < sizeof floppies / sizeof floppies[0]; i++) {
        if (floppies[i].size == size) {
            d->removable = 1;
            d->cylinders = floppies[i].cylinders;
            d->heads = floppies[i].heads;
            d->sectors = floppies[i].sectors;
            snprintf(d->label, sizeof d->label, "image %s", floppies[i].name);
            return 1;
        }
    }
    if (size > 0 && size % (16L * 63 * DISK_SECTOR) == 0) {
        d->heads = 16;
        d->sectors = 63;
    } else if (size > 0 && size % (4L * 17 * DISK_SECTOR) == 0) {
        d->heads = 4;
        d->sectors = 17;
    } else {
        return 0;
    }
    d->cylinders = (uint16_t)(size / ((long)d->heads * d->sectors * DISK_SECTOR));
    snprintf(d->label, sizeof d->label, "image fixed %ldK", size / 1024);
    return 1;
}

uint8_t disk_find(struct disk *disks, uint8_t max)
{
    const char *list = getenv("SPROCKET_DISKS");
    char all[1024];
    char *path;
    uint8_t n = 0;
    FILE *f;
    long size;

    if (list == NULL || strlen(list) >= sizeof all) {
        return 0;
    }
    strcpy(all, list);
    for (path = strtok(all, ":"); path != NULL && n < max; path = strtok(NULL, ":")) {
        f = fopen(path, "rb");
        if (f == NULL || strlen(path) >= sizeof paths[0]) {
            if (f != NULL) {
                fclose(f);
            }
            continue;
        }
        fseek(f, 0, SEEK_END);
        size = ftell(f);
        fclose(f);
        memset(&disks[n], 0, sizeof disks[n]);
        if (!geometry(&disks[n], size)) {
            continue;
        }
        disks[n].bios = (uint8_t)(disks[n].removable ? n : 0x80 + n);
        strcpy(paths[n], path);
        n++;
    }
    return n;
}

static int index_of(const struct disk *d)
{
    return d->removable ? d->bios : d->bios - 0x80;
}

uint8_t disk_read(const struct disk *d, uint16_t cylinder, uint8_t head, uint8_t sector,
                  uint8_t count, uint8_t *buf)
{
    FILE *f = fopen(paths[index_of(d)], "rb");
    long unit = ((long)cylinder * d->heads + head) * d->sectors + (sector - 1);
    size_t want = (size_t)count * DISK_SECTOR;
    size_t got = 0;

    if (f == NULL) {
        return 0x80;
    }
    if (fseek(f, unit * DISK_SECTOR, SEEK_SET) == 0) {
        got = fread(buf, 1, want, f);
    }
    fclose(f);
    /* Sector not found, past the end of the image. */
    return got == want ? DISK_OK : 0x04;
}

void disk_reset(const struct disk *d)
{
    (void)d;
}
