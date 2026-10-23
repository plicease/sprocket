/* SPDX-License-Identifier: MIT */
/*
 * The requester (req.h). Like the responder's main loop, the wait for a
 * response does nothing slower than a UART poll and the decoder per
 * received byte; the clock and the keyboard are checked only while the
 * line is idle.
 */
#include <string.h>

#include "frame.h"
#include "port.h"
#include "proto.h"
#include "req.h"

/* Retransmissions of one request before giving up (as the Linux side). */
#define RETRANSMITS 8
/* Check the clock and keyboard once per this many idle polls. */
#define IDLE_CHECK 256

uint8_t req_class;
uint16_t req_code;
char req_message[65];

static uint8_t *tx;
static struct decoder dec;
static uint16_t ours;
static uint16_t limit;
static long rate;
static int (*cancelled)(void);
static uint16_t session;
static uint8_t seq;
static uint32_t timeout;

static void put16(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)(v >> 8);
}

static uint16_t get16(const uint8_t *p)
{
    return (uint16_t)(p[0] | (p[1] << 8));
}

static void send_byte(uint8_t byte)
{
    port_put(byte);
}

/* Ticks for ms milliseconds, at least one. */
static uint32_t ticks(uint32_t ms)
{
    return ms * port_ticks_per_second() / 1000 + 1;
}

void req_init(uint8_t *tx_buf, uint8_t *rx_buf, uint16_t max_payload, long baud,
              int (*cancel)(void))
{
    tx = tx_buf;
    decoder_init(&dec, rx_buf, max_payload);
    ours = max_payload;
    limit = FRAME_MIN_PAYLOAD;
    rate = baud > 0 ? baud : 9600;
    cancelled = cancel;
}

uint8_t *req_payload(void)
{
    return tx + FRAME_HEADER_LEN;
}

uint16_t req_limit(void)
{
    return limit;
}

/*
 * Wait up to wait ticks for the response to the request in tx. Returns
 * REQ_OK with the frame in dec, REQ_LOST on timeout, or REQ_CANCELLED.
 */
static int await(uint32_t wait)
{
    uint32_t start = port_ticks();
    uint32_t last_byte = start;
    uint32_t now;
    uint32_t inter_byte = port_ticks_per_second();
    uint16_t idle = 0;
    uint8_t received = 0;
    uint8_t byte;
    const uint8_t *b;

    for (;;) {
        if (port_get(&byte)) {
            idle = 0;
            received = 1;
            if (decoder_push(&dec, byte) != FRAME_OK) {
                continue;
            }
            b = dec.buf;
            if (dec.frame_len < FRAME_HEADER_LEN || get16(b + 1) != session || b[3] != seq) {
                continue;
            }
            if (b[0] == MSG_PENDING) {
                start = port_ticks();
            } else if (b[0] == (tx[0] | MSG_RESPONSE) || b[0] == MSG_ERROR) {
                return REQ_OK;
            }
            continue;
        }
        if (++idle < IDLE_CHECK) {
            continue;
        }
        idle = 0;
        now = port_ticks();
        if (received) {
            received = 0;
            last_byte = now;
        } else if (decoder_in_frame(&dec) && now - last_byte > inter_byte) {
            decoder_timeout(&dec);
        }
        if (now - start > wait) {
            return REQ_LOST;
        }
        if (cancelled != 0 && cancelled()) {
            return REQ_CANCELLED;
        }
    }
}

/* Send the request in tx and wait, retransmitting; the next seq after. */
static int exchange(uint16_t len, uint32_t wait, uint16_t attempts)
{
    uint16_t attempt;
    int rc = REQ_LOST;

    tx[1] = (uint8_t)(session & 0xFF);
    tx[2] = (uint8_t)(session >> 8);
    tx[3] = seq;
    for (attempt = 0; attempt < attempts; attempt++) {
        frame_send(tx, (uint16_t)(FRAME_HEADER_LEN + len), send_byte);
        rc = await(wait);
        if (rc != REQ_LOST) {
            break;
        }
    }
    seq++;
    return rc;
}

/* Take an ERROR response apart. */
static int refused(const uint8_t *p, uint16_t len)
{
    uint16_t n;

    if (len < 4 || (uint16_t)(4 + p[3]) > len) {
        return REQ_BAD;
    }
    req_class = p[0];
    req_code = get16(p + 1);
    n = p[3] < sizeof req_message - 1 ? p[3] : sizeof req_message - 1;
    memcpy(req_message, p + 4, n);
    req_message[n] = '\0';
    return REQ_REFUSED;
}

int req_hello(uint16_t wait, struct req_server *server)
{
    static const uint8_t magic[4] = { 'S', 'P', 'R', 'K' };
    uint8_t *p = req_payload();
    const uint8_t *r;
    uint16_t len;
    uint32_t quiet = ticks(250);
    uint32_t start;
    uint8_t byte;
    uint16_t idle = 0;
    int rc;

    /* Drain: discard input until the line has been quiet for a moment. */
    start = port_ticks();
    for (;;) {
        if (port_get(&byte)) {
            start = port_ticks();
            continue;
        }
        if (++idle < IDLE_CHECK) {
            continue;
        }
        idle = 0;
        if (port_ticks() - start > quiet) {
            break;
        }
    }
    decoder_timeout(&dec);

    /* A new session ID: nonzero, and different from the last one. */
    session = (uint16_t)(session * 31 + 0x2B5 + port_ticks());
    if (session == 0) {
        session = 1;
    }
    seq = 0;
    tx[0] = MSG_HELLO;
    memcpy(p, magic, 4);
    p[4] = PROTO_MAJOR;
    p[5] = PROTO_MINOR;
    put16(p + 6, ours);
    rc = exchange(8, ticks(1000), wait > 0 ? wait : 1);
    if (rc != REQ_OK) {
        return rc;
    }
    r = dec.buf + FRAME_HEADER_LEN;
    len = (uint16_t)(dec.frame_len - FRAME_HEADER_LEN);
    if (dec.buf[0] == MSG_ERROR) {
        return refused(r, len);
    }
    if (len < 14 || memcmp(r, magic, 4) != 0 || r[4] != PROTO_MAJOR || (uint16_t)(14 + r[13]) > len
        || r[13] > 32 || get16(r + 6) < FRAME_MIN_PAYLOAD) {
        return REQ_BAD;
    }
    server->minor = r[5] < PROTO_MINOR ? r[5] : PROTO_MINOR;
    server->max_payload = get16(r + 6);
    server->capabilities = (uint32_t)get16(r + 8) | ((uint32_t)get16(r + 10) << 16);
    server->platform = r[12];
    memcpy(server->version, r + 14, r[13]);
    server->version[r[13]] = '\0';
    limit = server->max_payload < ours ? server->max_payload : ours;
    /* 0.5 s plus twice the wire time of the largest request and response. */
    timeout = ticks(500 + (uint32_t)(2L * (limit + 8) * 2 * 10 * 1000 / rate));
    return REQ_OK;
}

int req_call(uint8_t kind, uint16_t len, const uint8_t **reply, uint16_t *reply_len)
{
    int rc;

    tx[0] = kind;
    rc = exchange(len, timeout, RETRANSMITS + 1);
    if (rc != REQ_OK) {
        return rc;
    }
    *reply = dec.buf + FRAME_HEADER_LEN;
    *reply_len = (uint16_t)(dec.frame_len - FRAME_HEADER_LEN);
    if (dec.buf[0] == MSG_ERROR) {
        return refused(*reply, *reply_len);
    }
    return REQ_OK;
}
