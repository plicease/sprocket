/* SPDX-License-Identifier: MIT */
/*
 * Runs the C framing code against tests/vectors/frames.txt and
 * malformed.txt, the fixtures the Rust host is tested against.
 *
 * Usage: test_frame VECTOR_DIR
 */
#include <stdlib.h>
#include <string.h>

#include "../src/frame.h"
#include "vectors.h"

static uint8_t out[2 * VEC_MAX_BYTES + 8];
static uint16_t out_len;

static void collect(uint8_t byte)
{
    out[out_len++] = byte;
}

static uint8_t buf[FRAME_MAX_PAYLOAD + FRAME_HEADER_LEN + FRAME_CRC_LEN];

static const char *reason_name(int r)
{
    switch (r) {
    case FRAME_TOO_SHORT: return "too-short";
    case FRAME_BAD_CRC: return "bad-crc";
    case FRAME_BAD_ESCAPE: return "bad-escape";
    case FRAME_ABORTED: return "aborted";
    case FRAME_TOO_LONG: return "too-long";
    case FRAME_INCOMPLETE: return "incomplete";
    default: return "?";
    }
}

static FILE *open_fixture(const char *dir, const char *file)
{
    char path[512];
    FILE *f;

    snprintf(path, sizeof path, "%s/%s", dir, file);
    f = fopen(path, "r");
    if (!f) {
        printf("cannot open %s\n", path);
        exit(2);
    }
    return f;
}

static int valid(const char *dir)
{
    FILE *f = open_fixture(dir, "frames.txt");
    struct record r;
    struct decoder d;
    int count = 0;
    int results;
    uint16_t i;

    while (read_record(f, &r)) {
        count++;
        CHECK(crc16(r.body.data, r.body.len) == r.crc, "%s: crc", r.name);

        out_len = 0;
        frame_send(r.body.data, r.body.len, collect);
        CHECK(out_len == r.wire.len && memcmp(out, r.wire.data, out_len) == 0,
              "%s: encode", r.name);

        decoder_init(&d, buf, FRAME_MAX_PAYLOAD);
        results = 0;
        for (i = 0; i < r.wire.len; i++) {
            int res = decoder_push(&d, r.wire.data[i]);
            if (res == FRAME_NONE) {
                continue;
            }
            results++;
            CHECK(res == FRAME_OK, "%s: dropped (%s)", r.name, reason_name(res));
            CHECK(d.frame_len == r.body.len && memcmp(d.buf, r.body.data, d.frame_len) == 0,
                  "%s: decoded body", r.name);
            CHECK(d.crc == r.crc, "%s: decoded crc", r.name);
        }
        CHECK(results == 1, "%s: %d results", r.name, results);
    }
    fclose(f);
    return count;
}

static int malformed(const char *dir)
{
    FILE *f = open_fixture(dir, "malformed.txt");
    struct record r;
    struct decoder d;
    int count = 0;
    int n;
    uint16_t i;

    while (read_record(f, &r)) {
        count++;
        decoder_init(&d, buf, (uint16_t)(r.max ? r.max : FRAME_MAX_PAYLOAD));
        n = 0;
        for (i = 0; i < r.wire.len; i++) {
            int res = decoder_push(&d, r.wire.data[i]);
            if (res == FRAME_NONE) {
                continue;
            }
            if (n >= r.expect_count) {
                CHECK(0, "%s: unexpected result %d", r.name, res);
                n++;
                continue;
            }
            if (strcmp(r.expect[n], "frame") == 0) {
                CHECK(res == FRAME_OK, "%s: result %d: expected a frame, got %s",
                      r.name, n, reason_name(res));
                CHECK(res != FRAME_OK
                          || (d.frame_len == r.expect_body[n].len
                              && memcmp(d.buf, r.expect_body[n].data, d.frame_len) == 0),
                      "%s: result %d: body", r.name, n);
            } else {
                CHECK(res != FRAME_OK && strcmp(reason_name(res), r.expect[n]) == 0,
                      "%s: result %d: expected %s, got %s", r.name, n,
                      r.expect[n], res == FRAME_OK ? "a frame" : reason_name(res));
            }
            n++;
        }
        CHECK(n == r.expect_count, "%s: %d results, expected %d", r.name, n,
              r.expect_count);
    }
    fclose(f);
    return count;
}

static void timeout(void)
{
    static const uint8_t body[] = { 0x02, 0x3C, 0x5A, 0x01, 'p', 'i', 'n', 'g' };
    struct decoder d;
    uint16_t i;
    uint16_t half;

    out_len = 0;
    frame_send(body, sizeof body, collect);
    decoder_init(&d, buf, FRAME_MIN_PAYLOAD);

    CHECK(decoder_timeout(&d) == FRAME_NONE, "timeout before any frame");
    half = (uint16_t)(out_len / 2);
    for (i = 0; i < half; i++) {
        decoder_push(&d, out[i]);
    }
    CHECK(decoder_in_frame(&d), "in a frame after half of it");
    CHECK(decoder_timeout(&d) == FRAME_INCOMPLETE, "partial frame abandoned");
    /* The rest of the abandoned frame is noise until a delimiter. */
    for (i = half; i < out_len; i++) {
        CHECK(decoder_push(&d, out[i]) == FRAME_NONE, "tail byte %u", i);
    }
    for (i = 0; i < out_len; i++) {
        int res = decoder_push(&d, out[i]);
        CHECK(res == FRAME_NONE || (res == FRAME_OK && i == out_len - 1),
              "whole frame after timeout, byte %u", i);
    }
    CHECK(!decoder_in_frame(&d), "between frames");
}

int main(int argc, char **argv)
{
    int v;
    int m;

    if (argc != 2) {
        printf("usage: test_frame VECTOR_DIR\n");
        return 2;
    }
    CHECK(crc16((const uint8_t *)"123456789", 9) == 0x29B1, "CRC check value");
    v = valid(argv[1]);
    m = malformed(argv[1]);
    timeout();
    CHECK(v >= 6 && m >= 10, "fixture counts %d, %d", v, m);
    printf("test_frame: %d valid and %d malformed fixtures, %d failures\n", v,
           m, failures);
    return failures != 0;
}
