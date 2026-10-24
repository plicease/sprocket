/* SPDX-License-Identifier: MIT */
/*
 * The platform's disks, as far as imaging needs them: what is there, and
 * reading sectors by cylinder, head and sector. biosdisk.c uses BIOS INT
 * 13h; imgdisk.c offers image files so the same code can be tested on
 * Linux. Sectors are 512 bytes.
 */
#ifndef SPROCKET_DISK_H
#define SPROCKET_DISK_H

#include <stdint.h>

/* Most disks offered. */
#define DISK_MAX 8
#define DISK_LABEL_MAX 32
#define DISK_SECTOR 512

/* Statuses besides the BIOS's own (INT 13h AH). */
#define DISK_OK 0x00
/* The buffer would cross a 64 KB DMA boundary: read fewer sectors. */
#define DISK_DMA_BOUNDARY 0x09
/* Data read, but corrected by ECC: good, and suspect. */
#define DISK_CORRECTED 0x11

struct disk {
    uint8_t bios; /* BIOS drive number */
    uint8_t removable;
    /* Geometry of the medium; cylinders is 0 when there is none (no disk
     * in the drive, or it could not be determined). For a floppy it is
     * probed, so it can be wrong. */
    uint16_t cylinders;
    uint8_t heads;
    uint8_t sectors;
    /* The drive's largest geometry, as the BIOS reports it: reads are
     * allowed anywhere within it. */
    uint16_t max_cylinders;
    uint8_t max_heads;
    uint8_t max_sectors;
    char label[DISK_LABEL_MAX + 1];
};

/* Find the disks: fill disks[0 ..] and return how many, at most max. */
uint8_t disk_find(struct disk *disks, uint8_t max);

/*
 * Read count sectors from one track, starting at sector (numbered from 1),
 * into buf in one operation. Returns DISK_OK, DISK_CORRECTED, or the
 * platform's error status; DISK_DMA_BOUNDARY only when count > 1.
 */
uint8_t disk_read(const struct disk *d, uint16_t cylinder, uint8_t head, uint8_t sector,
                  uint8_t count, uint8_t *buf);

/* Reset the disk system after an error, before trying again. */
void disk_reset(const struct disk *d);

#endif
