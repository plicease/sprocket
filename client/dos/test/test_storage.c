/* SPDX-License-Identifier: MIT */
/*
 * Runs STORAGE_LIST and STORAGE_READ (src/storage.c) over a stub disk
 * layer: descriptors, whole-track reads, the fall back to one sector at a
 * time with retries and resets when a read fails, corrected (suspect)
 * reads, and refusals.
 *
 * Usage: test_storage
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/disk.h"
#include "../src/proto.h"
#include "../src/storage.h"

#define LIMIT 4096

static int failures;

#define CHECK(cond)                                                                \
    do {                                                                           \
        if (!(cond)) {                                                             \
            fprintf(stderr, "%s:%d: check failed: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                            \
        }                                                                          \
    } while (0)

/* The stub disks: a 360K floppy in a 1.2M drive, and a small fixed disk. */
static struct disk stub[2] = {
    {0x00, 1, 40, 2, 9, 80, 2, 15, "BIOS 00 (A:) 360K"},
    {0x80, 0, 100, 4, 17, 100, 4, 17, "BIOS 80 fixed"},
};
static uint8_t stub_count = 2;

/* Sector (c, h, s) on bad fails this many times with this status; the
 * whole-track read containing it fails with it too while it does. */
static struct {
    uint16_t c;
    uint8_t h, s;
    uint8_t times;
    uint8_t status;
} bad;
/* Reads of more than one sector return this status (e.g. a DMA
 * boundary), once. */
static uint8_t multi_status;
static uint8_t corrected; /* status for every successful read */
static int reads, resets;

uint8_t disk_find(struct disk *disks, uint8_t max)
{
    uint8_t n = stub_count < max ? stub_count : max;

    memcpy(disks, stub, n * sizeof *disks);
    return n;
}

static uint8_t stamp(uint16_t c, uint8_t h, uint8_t s)
{
    return (uint8_t)(c * 7 + h * 3 + s);
}

uint8_t disk_read(const struct disk *d, uint16_t cylinder, uint8_t head, uint8_t sector,
                  uint8_t count, uint8_t *buf)
{
    uint8_t i;

    (void)d;
    reads++;
    if (count > 1 && multi_status) {
        uint8_t status = multi_status;
        multi_status = 0;
        return status;
    }
    if (bad.times && cylinder == bad.c && head == bad.h && bad.s >= sector
        && bad.s < sector + count) {
        if (count == 1) {
            bad.times--;
        }
        return bad.status;
    }
    for (i = 0; i < count; i++) {
        memset(buf + i * DISK_SECTOR, stamp(cylinder, head, (uint8_t)(sector + i)),
               DISK_SECTOR);
    }
    return corrected;
}

void disk_reset(const struct disk *d)
{
    (void)d;
    resets++;
}

static uint8_t out[LIMIT];
static uint8_t class;

static int request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint16_t limit)
{
    uint16_t code;

    class = 0;
    reads = resets = 0;
    return storage_request(kind, in, in_len, out, limit, &class, &code);
}

static int read_chs(uint8_t device, uint16_t c, uint8_t h, uint8_t s, uint8_t count,
                    uint8_t retries, uint16_t limit)
{
    uint8_t in[8];

    in[0] = device;
    in[1] = 2; /* CHS */
    in[2] = (uint8_t)c;
    in[3] = (uint8_t)(c >> 8);
    in[4] = h;
    in[5] = s;
    in[6] = count;
    in[7] = retries;
    return request(MSG_STORAGE_READ, in, sizeof in, limit);
}

static uint16_t get16(const uint8_t *p)
{
    return (uint16_t)(p[0] | (p[1] << 8));
}

static int all(const uint8_t *p, uint8_t value)
{
    int i;

    for (i = 0; i < DISK_SECTOR; i++) {
        if (p[i] != value) {
            return 0;
        }
    }
    return 1;
}

static void test_list(void)
{
    int n;
    const uint8_t *p;

    CHECK(storage_init() == 2);
    n = request(MSG_STORAGE_LIST, NULL, 0, LIMIT);
    CHECK(n == 1 + 2 * 26 + 17 + 13);
    CHECK(out[0] == 2);
    p = out + 1;
    CHECK(p[0] == 25 + 17);
    CHECK(p[1] == 0 && p[2] == 1 && p[3] == 0x02);
    CHECK(get16(p + 4) == 512);
    CHECK(get16(p + 6) == 720 && get16(p + 8) == 0);
    CHECK(get16(p + 10) == 40 && p[12] == 2 && p[13] == 9 && p[14] == 1);
    CHECK(p[15] == 9 && p[16] == 1 && p[17] == 1 && p[18] == 0x00);
    /* The drive's geometry, beside the probed medium's. */
    CHECK(p[19] == 2 && p[20] == 4 && get16(p + 21) == 80 && p[23] == 2 && p[24] == 15);
    CHECK(p[25] == 17 && memcmp(p + 26, "BIOS 00 (A:) 360K", 17) == 0);
    p += 1 + p[0];
    CHECK(p[1] == 1 && p[2] == 2);
    CHECK(get16(p + 6) == 6800);
    CHECK(p[18] == 0x80);

    /* Too small for the descriptors. */
    CHECK(request(MSG_STORAGE_LIST, NULL, 0, 40) == -1);
    CHECK(class == ERR_INVALID_REQUEST);

    /* No geometry: no CHS, and no units. */
    stub_count = 1;
    stub[0].cylinders = 0;
    request(MSG_STORAGE_LIST, NULL, 0, LIMIT);
    CHECK(out[0] == 1 && out[4] == 0 && get16(out + 7) == 0);
    stub[0].cylinders = 40;
    stub_count = 2;
    CHECK(storage_init() == 2);
}

static void test_track_read(void)
{
    int n;
    int i;

    n = read_chs(1, 3, 2, 5, 7, 3, LIMIT);
    CHECK(n == 1 + 7 * (3 + 512));
    CHECK(out[0] == 7);
    CHECK(reads == 1 && resets == 0);
    for (i = 0; i < 7; i++) {
        CHECK(out[1 + 3 * i] == 0 && get16(out + 2 + 3 * i) == 0);
        CHECK(all(out + 1 + 3 * 7 + i * 512, stamp(3, 2, (uint8_t)(5 + i))));
    }

    /* Corrected: data, and every unit suspect. */
    corrected = DISK_CORRECTED;
    n = read_chs(0, 0, 0, 1, 2, 3, LIMIT);
    corrected = 0;
    CHECK(n == 1 + 2 * 515);
    CHECK(out[1] == 0 && get16(out + 2) == DISK_CORRECTED);
    CHECK(out[4] == 0 && get16(out + 5) == DISK_CORRECTED);
}

static void test_fallback(void)
{
    int n;

    /* Sector 3 fails twice: the track read fails, then sector by sector
     * with retries, sector 3 reads on its third attempt. */
    bad.c = 1;
    bad.h = 1;
    bad.s = 3;
    bad.times = 2;
    bad.status = 0x10;
    n = read_chs(0, 1, 1, 1, 4, 3, LIMIT);
    CHECK(n == 1 + 4 * 515);
    CHECK(reads == 1 + 1 + 1 + 3 + 1);
    CHECK(resets == 6);
    CHECK(out[1 + 3 * 2] == 0 && get16(out + 2 + 3 * 2) == 0);
    CHECK(all(out + 1 + 12 + 2 * 512, stamp(1, 1, 3)));

    /* Fails more times than the retries: no data for that unit, and the
     * others' data packed together. */
    bad.times = 5;
    n = read_chs(0, 1, 1, 2, 3, 1, LIMIT);
    CHECK(n == 1 + 3 * 3 + 2 * 512);
    CHECK(out[1] == 0);
    CHECK(out[4] == ERR_MEDIA_ERROR && get16(out + 5) == 0x10);
    CHECK(out[7] == 0);
    CHECK(all(out + 10, stamp(1, 1, 2)));
    CHECK(all(out + 10 + 512, stamp(1, 1, 4)));
    CHECK(bad.times == 3);
    bad.times = 0;

    /* A DMA boundary on the track read: one at a time, all good. */
    multi_status = DISK_DMA_BOUNDARY;
    n = read_chs(1, 0, 0, 1, 3, 0, LIMIT);
    CHECK(n == 1 + 3 * 515);
    CHECK(reads == 4 && out[1] == 0 && out[4] == 0 && out[7] == 0);
}

/* Past the probed medium but within the drive: the host may know the
 * disk better than the probe. */
static void test_beyond_the_probe(void)
{
    CHECK(read_chs(0, 79, 1, 10, 6, 0, LIMIT) == 1 + 6 * 515);
    CHECK(all(out + 1 + 18, stamp(79, 1, 10)));
}

static void test_refusals(void)
{
    uint8_t linear[10] = {0, 1, 0, 0, 0, 0, 1, 0, 0, 0};

    CHECK(request(MSG_STORAGE_READ, linear, sizeof linear, LIMIT) == -1);
    CHECK(class == ERR_UNSUPPORTED);
    CHECK(request(MSG_STORAGE_READ, linear, 1, LIMIT) == -1);
    CHECK(class == ERR_INVALID_REQUEST);
    CHECK(read_chs(2, 0, 0, 1, 1, 0, LIMIT) == -1);  /* no such device */
    CHECK(read_chs(0, 80, 0, 1, 1, 0, LIMIT) == -1); /* past the cylinders */
    CHECK(read_chs(0, 0, 2, 1, 1, 0, LIMIT) == -1);  /* past the heads */
    CHECK(read_chs(0, 0, 0, 0, 1, 0, LIMIT) == -1);  /* sector 0 */
    CHECK(read_chs(0, 0, 0, 14, 3, 0, LIMIT) == -1); /* off the track */
    CHECK(read_chs(0, 0, 0, 1, 0, 0, LIMIT) == -1);  /* nothing */
    CHECK(class == ERR_INVALID_REQUEST);
    CHECK(reads == 0);
    /* Two units need 1 + 2 * 515 bytes. */
    CHECK(read_chs(0, 0, 0, 1, 2, 0, 1030) == -1);
    CHECK(read_chs(0, 0, 0, 1, 2, 0, 1031) == 1031);
    CHECK(request(0x7F, NULL, 0, LIMIT) == -1 && class == ERR_UNSUPPORTED);
}

int main(void)
{
    test_list();
    test_track_read();
    test_fallback();
    test_beyond_the_probe();
    test_refusals();
    if (failures) {
        fprintf(stderr, "test_storage: %d failure(s)\n", failures);
        return 1;
    }
    printf("test_storage: ok\n");
    return 0;
}
