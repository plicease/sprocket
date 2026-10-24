/* SPDX-License-Identifier: MIT */
/*
 * Runs the client protocol core against the request/response pairs in
 * tests/vectors/frames.txt, plus the session and duplicate rules.
 *
 * Usage: test_proto VECTOR_DIR
 */
#include <stdlib.h>
#include <string.h>

#include "../src/frame.h"
#include "../src/proto.h"
#include "vectors.h"

static char frames_path[512];
static uint8_t resp_buf[512 + FRAME_HEADER_LEN];
static struct client cl;

/* Hand a request body to the client; returns the response length, or -1
 * if it was dropped. */
static int handle(const uint8_t *body, uint16_t len, const uint8_t **out)
{
    uint16_t out_len;

    if (!client_handle(&cl, body, len, crc16(body, len), out, &out_len)) {
        return -1;
    }
    return out_len;
}

/* The response to fixture `request` must be fixture `response`. */
static void exchange(const char *request, const char *response)
{
    struct record req;
    struct record resp;
    const uint8_t *out;
    int len;

    find_record(frames_path, request, &req);
    find_record(frames_path, response, &resp);
    len = handle(req.body.data, req.body.len, &out);
    CHECK(len == resp.body.len && memcmp(out, resp.body.data, len) == 0,
          "%s -> %s", request, response);
}

static void reset(void)
{
    /* Matches the HELLO_REPLY fixture. */
    cl.max_payload = 512;
    cl.capabilities = 3;
    cl.platform = PLATFORM_DOS;
    cl.version = "0.1.0";
    client_init(&cl, resp_buf);
}

static void fixtures(void)
{
    reset();
    exchange("hello", "hello_reply");
    CHECK(cl.started && cl.session == 0x5A3C, "session started");
    exchange("ping", "ping_reply");
    /* seq 2, type 0x40: not a v1 request. */
    {
        static const uint8_t unknown[] = { 0x40, 0x3C, 0x5A, 0x02 };
        struct record err;
        const uint8_t *out;
        int len = handle(unknown, sizeof unknown, &out);
        find_record(frames_path, "error", &err);
        CHECK(len == err.body.len && memcmp(out, err.body.data, len) == 0,
              "unknown request -> error fixture");
    }
}

static void sessions_and_duplicates(void)
{
    static const uint8_t hello[] = { 0x01, 0x34, 0x12, 0x00, 'S', 'P', 'R', 'K',
                                     1, 0, 0x00, 0x04 };
    static const uint8_t ping_a[] = { 0x02, 0x34, 0x12, 0x01, 'a' };
    static const uint8_t ping_b[] = { 0x02, 0x34, 0x12, 0x01, 'b' };
    static const uint8_t old_session[] = { 0x02, 0x35, 0x12, 0x02, 'x' };
    static const uint8_t close[] = { 0x03, 0x34, 0x12, 0x02 };
    static const uint8_t no_session[] = { 0x02, 0x00, 0x00, 0x00 };
    const uint8_t *out;
    uint8_t first[sizeof ping_a];
    int len;

    reset();
    CHECK(handle(ping_a, sizeof ping_a, &out) == -1, "request before HELLO dropped");
    CHECK(handle(no_session, sizeof no_session, &out) == -1, "session 0 dropped");

    CHECK(handle(hello, sizeof hello, &out) > 0, "HELLO answered");
    cl.started = 0;
    CHECK(cl.host_max_payload == 1024, "host maximum recorded");
    /* A repeated HELLO is a duplicate: the same reply, no new session. */
    len = handle(hello, sizeof hello, &out);
    CHECK(len > 0 && out[0] == 0x81 && !cl.started, "repeated HELLO from cache");

    len = handle(ping_a, sizeof ping_a, &out);
    CHECK(len == sizeof ping_a && out[0] == 0x82 && out[4] == 'a', "ping a");
    memcpy(first, out, sizeof first);
    len = handle(ping_a, sizeof ping_a, &out);
    CHECK(len == sizeof ping_a && memcmp(out, first, len) == 0 && cl.duplicates == 2,
          "duplicate ping from cache");

    /* Same seq, different payload: refused, and the cache survives. */
    len = handle(ping_b, sizeof ping_b, &out);
    CHECK(len == 8 && out[0] == 0xFF && out[4] == ERR_INVALID_REQUEST,
          "same seq, different request refused");
    len = handle(ping_a, sizeof ping_a, &out);
    CHECK(len == sizeof ping_a && memcmp(out, first, len) == 0,
          "cache kept after a refusal");

    CHECK(handle(old_session, sizeof old_session, &out) == -1,
          "other session dropped");

    /* A HELLO with the current session after later requests restarts. */
    len = handle(hello, sizeof hello, &out);
    CHECK(len > 0 && cl.started, "HELLO after later requests starts a session");
    cl.started = 0;

    len = handle(close, sizeof close, &out);
    CHECK(len == FRAME_HEADER_LEN && out[0] == 0x83 && cl.closed, "CLOSE");
}

static void version_mismatch(void)
{
    static const uint8_t hello2[] = { 0x01, 0x34, 0x12, 0x00, 'S', 'P', 'R', 'K',
                                      2, 0, 0x00, 0x04 };
    static const uint8_t bad_magic[] = { 0x01, 0x35, 0x12, 0x00, 'X', 'P', 'R', 'K',
                                         1, 0, 0x00, 0x04 };
    const uint8_t *out;
    int len;

    reset();
    len = handle(hello2, sizeof hello2, &out);
    CHECK(len == 8 && out[0] == 0xFF && out[4] == ERR_PROTOCOL_MISMATCH
              && out[5] == PROTO_MAJOR && out[6] == cl.minor,
          "major 2 -> PROTOCOL_MISMATCH with the client's version");
    len = handle(bad_magic, sizeof bad_magic, &out);
    CHECK(len == 8 && out[4] == ERR_INVALID_REQUEST, "bad magic refused");
}

int main(int argc, char **argv)
{
    if (argc != 2) {
        printf("usage: test_proto VECTOR_DIR\n");
        return 2;
    }
    snprintf(frames_path, sizeof frames_path, "%s/frames.txt", argv[1]);
    fixtures();
    sessions_and_duplicates();
    version_mismatch();
    printf("test_proto: %d failures\n", failures);
    return failures != 0;
}
