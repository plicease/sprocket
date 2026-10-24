/* SPDX-License-Identifier: MIT */
/*
 * BIOS disks (INT 13h), for imaging. Floppy drives come from the
 * equipment word and hard disks from the count in the BIOS data area;
 * each drive's geometry from function 08h, which the original 1981 PC/XT
 * BIOS lacks, so drives it cannot describe are left out.
 *
 * For a floppy, function 08h gives the drive's geometry, not the disk's:
 * a 360K disk in a 1.2M drive reports 15 sectors a track. The inserted
 * disk is probed by reading the last sector of the first track for each
 * standard size, largest first.
 *
 * INT 13h fails (status 09h) when a read would cross a 64 KB physical
 * boundary, which depends on where DOS loaded the program. A multi-sector
 * read that would cross one returns DISK_DMA_BOUNDARY so the caller reads
 * sector by sector, and a single sector that would cross one goes through
 * a bounce buffer.
 */
#include <dos.h>
#include <i86.h>
#include <stdio.h>
#include <string.h>

#include "disk.h"

#define STATUS_TIMEOUT 0x80

/* Twice a sector, so one half never crosses a 64 KB boundary. */
static uint8_t bounce[2 * DISK_SECTOR];

static unsigned long physical(void far *p)
{
    return ((unsigned long)FP_SEG(p) << 4) + FP_OFF(p);
}

/* bytes from p would cross a 64 KB physical boundary. */
static int crosses(void far *p, unsigned long bytes)
{
    unsigned long start = physical(p);

    return (start >> 16) != ((start + bytes - 1) >> 16);
}

static uint8_t bios_read(uint8_t drive, uint16_t cylinder, uint8_t head, uint8_t sector,
                         uint8_t count, void far *buf)
{
    union REGS r;
    struct SREGS s;

    segread(&s);
    r.h.ah = 0x02;
    r.h.al = count;
    r.h.ch = (uint8_t)cylinder;
    r.h.cl = (uint8_t)(sector | ((cylinder >> 2) & 0xC0));
    r.h.dh = head;
    r.h.dl = drive;
    s.es = FP_SEG(buf);
    r.x.bx = FP_OFF(buf);
    int86x(0x13, &r, &r, &s);
    return r.x.cflag ? r.h.ah : DISK_OK;
}

uint8_t disk_read(const struct disk *d, uint16_t cylinder, uint8_t head, uint8_t sector,
                  uint8_t count, uint8_t *buf)
{
    void far *target = buf;
    uint8_t *half;
    uint8_t status;

    if (!crosses(target, (unsigned long)count * DISK_SECTOR)) {
        return bios_read(d->bios, cylinder, head, sector, count, target);
    }
    if (count > 1) {
        return DISK_DMA_BOUNDARY;
    }
    half = crosses((void far *)bounce, DISK_SECTOR) ? bounce + DISK_SECTOR : bounce;
    status = bios_read(d->bios, cylinder, head, sector, 1, (void far *)half);
    if (status == DISK_OK || status == DISK_CORRECTED) {
        memcpy(buf, half, DISK_SECTOR);
    }
    return status;
}

void disk_reset(const struct disk *d)
{
    union REGS r;

    r.h.ah = 0x00;
    r.h.dl = d->bios;
    int86(0x13, &r, &r);
}

/* Function 08h: the drive's geometry. Returns 0 if the BIOS cannot say. */
static int parameters(uint8_t drive, struct disk *d)
{
    union REGS r;
    struct SREGS s;

    segread(&s);
    r.h.ah = 0x08;
    r.h.dl = drive;
    r.x.di = 0;
    int86x(0x13, &r, &r, &s);
    if (r.x.cflag || (r.h.cl & 0x3F) == 0) {
        return 0;
    }
    d->cylinders = (uint16_t)((r.h.ch | ((r.h.cl & 0xC0) << 2)) + 1);
    d->heads = (uint8_t)(r.h.dh + 1);
    d->sectors = (uint8_t)(r.h.cl & 0x3F);
    return 1;
}

/* Read sector `sector` of the first track, with a retry for the motor
 * starting and a disk change. */
static int readable(const struct disk *d, uint8_t sector)
{
    int attempt;
    uint8_t status;

    for (attempt = 0; attempt < 3; attempt++) {
        status = disk_read(d, 0, 0, sector, 1, bounce);
        if (status == DISK_OK || status == DISK_CORRECTED) {
            return 1;
        }
        disk_reset(d);
        if (status == STATUS_TIMEOUT) {
            /* No disk: the drive did not become ready. */
            return 0;
        }
    }
    return 0;
}

/* The inserted disk's geometry, within the drive's maximum. */
static void probe(struct disk *d)
{
    static const uint8_t sizes[] = { 36, 18, 15, 9, 8 };
    uint8_t drive_sectors = d->sectors;
    unsigned i;

    for (i = 0; i < sizeof sizes; i++) {
        if (sizes[i] > drive_sectors || !readable(d, sizes[i])) {
            continue;
        }
        d->sectors = sizes[i];
        /* 8 or 9 sectors in a high-density 5.25" drive: a 40-track disk. */
        if (sizes[i] <= 9 && drive_sectors == 15) {
            d->cylinders = 40;
        }
        return;
    }
    d->cylinders = 0;
}

static void floppy_label(struct disk *d)
{
    unsigned long kb = (unsigned long)d->cylinders * d->heads * d->sectors / 2;

    if (d->cylinders == 0) {
        sprintf(d->label, "BIOS %02X (%c:) no disk", d->bios, 'A' + d->bios);
    } else if (kb >= 1000) {
        /* Floppy sizes are named in thousands of kilobytes: 1440K is 1.44M. */
        sprintf(d->label, "BIOS %02X (%c:) %lu.%02luM", d->bios, 'A' + d->bios, kb / 1000,
                kb % 1000 / 10);
    } else {
        sprintf(d->label, "BIOS %02X (%c:) %luK", d->bios, 'A' + d->bios, kb);
    }
}

uint8_t disk_find(struct disk *disks, uint8_t max)
{
    union REGS r;
    uint8_t n = 0;
    uint8_t floppies;
    uint8_t fixed;
    uint8_t i;
    unsigned long units;
    struct disk *d;

    int86(0x11, &r, &r);
    floppies = (uint8_t)((r.x.ax & 1) ? ((r.x.ax >> 6) & 3) + 1 : 0);
    for (i = 0; i < floppies && n < max; i++) {
        d = &disks[n];
        memset(d, 0, sizeof *d);
        d->bios = i;
        d->removable = 1;
        if (!parameters(i, d)) {
            continue;
        }
        probe(d);
        floppy_label(d);
        n++;
    }

    fixed = *(uint8_t far *)MK_FP(0x40, 0x75);
    for (i = 0; i < fixed && n < max; i++) {
        d = &disks[n];
        memset(d, 0, sizeof *d);
        d->bios = (uint8_t)(0x80 + i);
        if (!parameters(d->bios, d)) {
            continue;
        }
        units = (unsigned long)d->cylinders * d->heads * d->sectors;
        sprintf(d->label, "BIOS %02X fixed %lu.%luM", d->bios, units / 2048,
                (units % 2048) * 10 / 2048);
        n++;
    }
    return n;
}
