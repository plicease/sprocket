/* SPDX-License-Identifier: MIT */
#include <string.h>

#include "frame.h"
#include "proto.h"

static const uint8_t magic[4] = { 'S', 'P', 'R', 'K' };

/* An ERROR that must not replace the cached response. */
static uint8_t refusal[FRAME_HEADER_LEN + 4];

void client_init(struct client *c, uint8_t *resp)
{
    c->minor = 0;
    c->handler = 0;
    c->reset = 0;
    c->session = 0;
    c->host_max_payload = FRAME_MIN_PAYLOAD;
    c->session_minor = 0;
    c->have_record = 0;
    c->resp = resp;
    c->resp_len = 0;
    c->started = 0;
    c->closed = 0;
    c->duplicates = 0;
}

static void put16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)(v >> 8);
}

static void put32(uint8_t *p, uint32_t v)
{
    put16(p, (uint16_t)(v & 0xFFFF));
    put16(p + 2, (uint16_t)(v >> 16));
}

/* Start a response body in buf; returns the payload pointer. */
static uint8_t *header(uint8_t *buf, uint8_t kind, const uint8_t *request)
{
    buf[0] = kind;
    buf[1] = request[1];
    buf[2] = request[2];
    buf[3] = request[3];
    return buf + FRAME_HEADER_LEN;
}

/* ERROR with no message; returns the body length. Classes from minor
 * version 1 become FILE_ERROR for a host that does not know them. */
static uint16_t error_body(uint8_t *buf, const uint8_t *request,
                           uint8_t class, uint16_t code, uint8_t minor)
{
    uint8_t *p = header(buf, MSG_ERROR, request);
    if (class > ERR_VERIFY_FAILED && minor == 0) {
        class = ERR_FILE_ERROR;
    }
    p[0] = class;
    put16(p + 1, code);
    p[3] = 0; /* message_len */
    return FRAME_HEADER_LEN + 4;
}

static uint16_t hello(struct client *c, const uint8_t *body, uint16_t len)
{
    const uint8_t *in = body + FRAME_HEADER_LEN;
    uint16_t in_len = (uint16_t)(len - FRAME_HEADER_LEN);
    uint8_t *p;
    uint16_t version_len;

    if (in_len < 8 || memcmp(in, magic, 4) != 0) {
        return error_body(c->resp, body, ERR_INVALID_REQUEST, 0, 0);
    }
    if (in[4] != PROTO_MAJOR) {
        return error_body(c->resp, body, ERR_PROTOCOL_MISMATCH,
                          (uint16_t)(PROTO_MAJOR | (c->minor << 8)), 0);
    }
    c->host_max_payload = (uint16_t)(in[6] | (in[7] << 8));
    c->session_minor = in[5] < c->minor ? in[5] : c->minor;

    version_len = (uint16_t)strlen(c->version);
    if (version_len > 32) {
        version_len = 32;
    }
    p = header(c->resp, MSG_HELLO | MSG_RESPONSE, body);
    memcpy(p, magic, 4);
    p[4] = PROTO_MAJOR;
    p[5] = c->minor;
    put16(p + 6, c->max_payload);
    put32(p + 8, c->capabilities);
    p[12] = c->platform;
    p[13] = (uint8_t)version_len;
    memcpy(p + 14, c->version, version_len);
    return (uint16_t)(FRAME_HEADER_LEN + 14 + version_len);
}

static uint16_t execute(struct client *c, const uint8_t *body, uint16_t len)
{
    uint8_t *p;
    uint16_t limit;
    uint8_t class = ERR_UNSUPPORTED;
    uint16_t code = 0;
    int n;

    switch (body[0]) {
    case MSG_HELLO:
        return hello(c, body, len);
    case MSG_PING:
        p = header(c->resp, MSG_PING | MSG_RESPONSE, body);
        memcpy(p, body + FRAME_HEADER_LEN, len - FRAME_HEADER_LEN);
        return len;
    case MSG_CLOSE:
        header(c->resp, MSG_CLOSE | MSG_RESPONSE, body);
        if (c->reset) {
            c->reset();
        }
        c->closed = 1;
        return FRAME_HEADER_LEN;
    default:
        if (c->handler == 0) {
            return error_body(c->resp, body, ERR_UNSUPPORTED, 0, 0);
        }
        /* The host's limit and ours. */
        limit = c->host_max_payload < c->max_payload ? c->host_max_payload
                                                     : c->max_payload;
        p = header(c->resp, (uint8_t)(body[0] | MSG_RESPONSE), body);
        n = c->handler(body[0], body + FRAME_HEADER_LEN,
                       (uint16_t)(len - FRAME_HEADER_LEN), p, limit, &class, &code);
        if (n < 0) {
            return error_body(c->resp, body, class, code, c->session_minor);
        }
        return (uint16_t)(FRAME_HEADER_LEN + n);
    }
}

int client_handle(struct client *c, const uint8_t *body, uint16_t len,
                  uint16_t crc, const uint8_t **out, uint16_t *out_len)
{
    uint8_t kind = body[0];
    uint16_t session = (uint16_t)(body[1] | (body[2] << 8));
    uint8_t seq = body[3];
    int current = c->session != 0 && session == c->session;

    if (session == 0 || kind >= MSG_RESPONSE) {
        return 0;
    }
    if (kind != MSG_HELLO && !current) {
        return 0;
    }

    if (current && c->have_record) {
        if (seq == c->rec_seq && kind == c->rec_kind && len == c->rec_len
            && crc == c->rec_crc) {
            /*
             * A retransmission. Requests here finish before the next byte
             * is read, so the original is always complete: resend its
             * response without executing it again.
             */
            c->duplicates++;
            *out = c->resp;
            *out_len = c->resp_len;
            return 1;
        }
        if (seq == c->rec_seq && kind != MSG_HELLO) {
            /* Same seq, different request: refuse, keep the cache. */
            *out = refusal;
            *out_len = error_body(refusal, body, ERR_INVALID_REQUEST, 0, 0);
            return 1;
        }
    }

    if (kind == MSG_HELLO) {
        /* Any HELLO that is not a duplicate starts a new session. */
        if (c->reset) {
            c->reset();
        }
        c->session = session;
        c->started = 1;
    }
    c->have_record = 1;
    c->rec_seq = seq;
    c->rec_kind = kind;
    c->rec_len = len;
    c->rec_crc = crc;
    c->resp_len = execute(c, body, len);
    *out = c->resp;
    *out_len = c->resp_len;
    return 1;
}
