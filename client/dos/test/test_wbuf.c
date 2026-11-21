/* SPDX-License-Identifier: MIT */
/*
 * src/wbuf.c: the file gets exactly the bytes written, in whole blocks of
 * WBUF_SIZE until wbuf_end writes the rest, and a failed write is
 * reported.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/wbuf.h"

static int failures;

static void check(int ok, const char *what)
{
    if (!ok) {
        printf("FAIL: %s\n", what);
        failures++;
    }
}

/* Write total bytes in pieces of the given sizes (cycling), checking
 * after each piece that only whole blocks have reached the file. */
static void run(const uint16_t *sizes, size_t n_sizes, long total, const char *name)
{
    static uint8_t data[140000];
    static uint8_t back[140000];
    char what[128];
    FILE *f = tmpfile();
    long done = 0;
    long i;
    size_t k = 0;
    int aligned = 1;

    for (i = 0; i < total; i++) {
        data[i] = (uint8_t)(i * 7 + i / 251);
    }
    /* Unbuffered, so ftell is what has reached the file. */
    setvbuf(f, NULL, _IONBF, 0);
    wbuf_begin(f);
    while (done < total) {
        uint16_t n = sizes[k++ % n_sizes];

        if (n > total - done) {
            n = (uint16_t)(total - done);
        }
        if (!wbuf_write(data + done, n)) {
            snprintf(what, sizeof what, "%s: write at %ld failed", name, done);
            check(0, what);
            break;
        }
        done += n;
        if (ftell(f) % WBUF_SIZE != 0 || ftell(f) > done || done - ftell(f) >= WBUF_SIZE) {
            aligned = 0;
        }
    }
    snprintf(what, sizeof what, "%s: only whole blocks before the end", name);
    check(aligned, what);
    snprintf(what, sizeof what, "%s: wbuf_end", name);
    check(wbuf_end(), what);
    snprintf(what, sizeof what, "%s: length", name);
    check(ftell(f) == total, what);
    rewind(f);
    snprintf(what, sizeof what, "%s: contents", name);
    check(fread(back, 1, (size_t)total, f) == (size_t)total && memcmp(back, data, (size_t)total) == 0,
          what);
    fclose(f);
}

int main(void)
{
    static const uint16_t download[] = { 1016 };
    static const uint16_t mixed[] = { 1, 4095, 4096, 4097, 0, 513, 8192, 3 };
    static const uint16_t whole[] = { 4096 };
    static uint8_t junk[WBUF_SIZE];
    FILE *f;

    run(download, 1, 131072, "1016-byte payloads");
    run(download, 1, 1016 * 3 + 8, "a short last payload");
    run(mixed, sizeof mixed / sizeof mixed[0], 65536 + 1234, "mixed sizes");
    run(whole, 1, 3 * WBUF_SIZE, "whole blocks");
    run(download, 1, 0, "an empty file");

    /* A stream that cannot be written: the first full block fails, and so
     * does wbuf_end with bytes pending. */
    f = fopen("/dev/null", "rb");
    wbuf_begin(f);
    check(wbuf_write(junk, 100) == 1, "a partial block is only gathered");
    check(wbuf_write(junk, WBUF_SIZE) == 0, "a failed block write is reported");
    check(wbuf_write(junk, 10) == 1, "gathering continues after a failure");
    check(wbuf_end() == 0, "a failed final write is reported");
    fclose(f);

    /* wbuf_drop discards what is pending: nothing reaches the file. */
    f = tmpfile();
    setvbuf(f, NULL, _IONBF, 0);
    wbuf_begin(f);
    wbuf_write(junk, 100);
    wbuf_drop();
    check(ftell(f) == 0, "wbuf_drop writes nothing");
    fclose(f);

    if (failures == 0) {
        printf("test_wbuf: ok\n");
    }
    return failures != 0;
}
