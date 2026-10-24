/* SPDX-License-Identifier: MIT */
/*
 * STORAGE_LIST and STORAGE_READ over the platform's disks (disk.h).
 *
 * A read first asks for the whole run of sectors in one operation, which
 * is how a floppy keeps up with its own rotation. If that fails, each
 * sector is read on its own, with the retries the host asked for and a
 * reset between attempts, so that every unit gets its own status: the
 * host can then record exactly which units failed. Unreadable units send
 * no data; the client never fills them in.
 */
#include <stdio.h>
#include <string.h>

#include "disk.h"
#include "proto.h"
#include "storage.h"

#define KIND_REMOVABLE 1
#define KIND_FIXED 2
#define ADDR_CHS 0x02
#define MODE_LINEAR 1
#define MODE_CHS 2
#define META_BIOS_DRIVE 1
#define META_DRIVE_GEOMETRY 2
/* meta: the BIOS drive (tag, length, value) and the drive's geometry
 * (tag, length, cylinders u16, heads, sectors). */
#define META_LEN (3 + 6)

static struct disk disks[DISK_MAX];
static uint8_t disk_count;

uint8_t storage_init(void)
{
    disk_count = disk_find(disks, DISK_MAX);
    return disk_count;
}

static void put16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
}

static void put32(uint8_t *p, uint32_t v)
{
    put16(p, (uint16_t)v);
    put16(p + 2, (uint16_t)(v >> 16));
}

static int fail(uint8_t *class, uint8_t c)
{
    *class = c;
    return -1;
}

static uint32_t units(const struct disk *d)
{
    return (uint32_t)d->cylinders * d->heads * d->sectors;
}

/* Fixed fields of a descriptor, after its length byte, through
 * first_sector. */
#define DESC_FIXED 14

static int list_request(uint8_t *out, uint16_t limit, uint8_t *class)
{
    uint16_t used = 1;
    uint8_t i;
    uint8_t *p;
    size_t label;

    /* The disks again: a floppy may have been changed. */
    disk_count = disk_find(disks, DISK_MAX);
    out[0] = disk_count;
    for (i = 0; i < disk_count; i++) {
        label = strlen(disks[i].label);
        if (used + 1 + DESC_FIXED + 1 + META_LEN + 1 + label > limit) {
            return fail(class, ERR_INVALID_REQUEST);
        }
        p = out + used;
        p[0] = (uint8_t)(DESC_FIXED + 1 + META_LEN + 1 + label);
        p[1] = i;
        p[2] = disks[i].removable ? KIND_REMOVABLE : KIND_FIXED;
        p[3] = disks[i].cylinders ? ADDR_CHS : 0;
        put16(p + 4, DISK_SECTOR);
        put32(p + 6, units(&disks[i]));
        put16(p + 10, disks[i].cylinders);
        p[12] = disks[i].heads;
        p[13] = disks[i].sectors;
        p[14] = 1;
        p[15] = META_LEN;
        p[16] = META_BIOS_DRIVE;
        p[17] = 1;
        p[18] = disks[i].bios;
        p[19] = META_DRIVE_GEOMETRY;
        p[20] = 4;
        put16(p + 21, disks[i].max_cylinders);
        p[23] = disks[i].max_heads;
        p[24] = disks[i].max_sectors;
        p[25] = (uint8_t)label;
        memcpy(p + 26, disks[i].label, label);
        used = (uint16_t)(used + 1 + p[0]);
    }
    return used;
}

static int read_request(const uint8_t *in, uint16_t in_len, uint8_t *out, uint16_t limit,
                        uint8_t *class)
{
    const struct disk *d;
    uint16_t cylinder;
    uint8_t head;
    uint8_t sector;
    uint8_t count;
    uint8_t retries;
    uint8_t status;
    uint8_t attempt;
    uint8_t i;
    uint8_t *data;
    uint8_t *next;

    if (in_len < 2) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    if (in[1] == MODE_LINEAR) {
        return fail(class, ERR_UNSUPPORTED);
    }
    if (in[1] != MODE_CHS || in_len < 8 || in[0] >= disk_count) {
        return fail(class, ERR_INVALID_REQUEST);
    }
    d = &disks[in[0]];
    cylinder = (uint16_t)(in[2] | (in[3] << 8));
    head = in[4];
    sector = in[5];
    count = in[6];
    retries = in[7];
    /* Within the drive (the host may know the medium better than the
     * probe did), on one track, and fitting the response. */
    if (count == 0 || cylinder >= d->max_cylinders || head >= d->max_heads || sector == 0
        || sector - 1 + count > d->max_sectors
        || 1 + (uint32_t)count * (3 + DISK_SECTOR) > limit) {
        return fail(class, ERR_INVALID_REQUEST);
    }

    out[0] = count;
    data = out + 1 + 3 * count;
    status = disk_read(d, cylinder, head, sector, count, data);
    if (status == DISK_OK || status == DISK_CORRECTED) {
        for (i = 0; i < count; i++) {
            out[1 + 3 * i] = 0;
            /* Which sector was corrected is not known: all are suspect. */
            put16(out + 2 + 3 * i, status);
        }
        return 1 + count * (3 + DISK_SECTOR);
    }

    /* One sector at a time, for a status per unit. */
    next = data;
    for (i = 0; i < count; i++) {
        attempt = 0;
        do {
            disk_reset(d);
            status = disk_read(d, cylinder, head, (uint8_t)(sector + i), 1, next);
        } while (status != DISK_OK && status != DISK_CORRECTED && attempt++ < retries);
        if (status == DISK_OK || status == DISK_CORRECTED) {
            out[1 + 3 * i] = 0;
            next += DISK_SECTOR;
        } else {
            out[1 + 3 * i] = ERR_MEDIA_ERROR;
        }
        put16(out + 2 + 3 * i, status);
    }
    return (int)(next - out);
}

int storage_request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint8_t *out,
                    uint16_t limit, uint8_t *class, uint16_t *code)
{
    *code = 0;
    switch (kind) {
    case MSG_STORAGE_LIST:
        return list_request(out, limit, class);
    case MSG_STORAGE_READ:
        return read_request(in, in_len, out, limit, class);
    default:
        return fail(class, ERR_UNSUPPORTED);
    }
}
