/* SPDX-License-Identifier: MIT */
/* Gathered writes in whole blocks (wbuf.h). */
#include <string.h>

#include "wbuf.h"

static FILE *file;
static uint8_t block[WBUF_SIZE];
static uint16_t used;

void wbuf_begin(FILE *f)
{
    file = f;
    used = 0;
}

/* Write the pending bytes; 1, or 0 if fewer were written. */
static int flush(void)
{
    uint16_t n = used;

    used = 0;
    return n == 0 || fwrite(block, 1, n, file) == n;
}

int wbuf_write(const void *data, uint16_t n)
{
    const uint8_t *p = data;
    uint16_t k;

    while (n != 0) {
        k = (uint16_t)(WBUF_SIZE - used);
        if (k > n) {
            k = n;
        }
        memcpy(block + used, p, k);
        used = (uint16_t)(used + k);
        p += k;
        n = (uint16_t)(n - k);
        if (used == WBUF_SIZE && !flush()) {
            return 0;
        }
    }
    return 1;
}

int wbuf_end(void)
{
    int ok = flush();

    file = NULL;
    return ok;
}

void wbuf_drop(void)
{
    used = 0;
    file = NULL;
}
