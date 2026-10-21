/* SPDX-License-Identifier: MIT */
/* Reader for the plain-text fixtures in tests/vectors/. */
#ifndef SPROCKET_VECTORS_H
#define SPROCKET_VECTORS_H

#include <stdint.h>
#include <stdio.h>

#define VEC_MAX_BYTES 1024
#define VEC_MAX_EXPECT 8

struct bytes {
    uint8_t data[VEC_MAX_BYTES];
    uint16_t len;
};

struct record {
    char name[64];
    struct bytes body;
    struct bytes wire;
    unsigned crc;
    int has_crc;
    unsigned max;
    int expect_count;
    char expect[VEC_MAX_EXPECT][16]; /* "frame" or a drop reason */
    struct bytes expect_body[VEC_MAX_EXPECT];
};

/* Read the next record; returns 0 at end of file. */
int read_record(FILE *f, struct record *r);

/* Find a record by name in a fixture file; exits on failure. */
void find_record(const char *path, const char *name, struct record *r);

/* Test bookkeeping. */
extern int failures;
#define CHECK(cond, ...)                                                    \
    do {                                                                    \
        if (!(cond)) {                                                      \
            failures++;                                                     \
            printf("FAIL %s:%d: ", __FILE__, __LINE__);                     \
            printf(__VA_ARGS__);                                            \
            printf("\n");                                                   \
        }                                                                   \
    } while (0)

#endif
