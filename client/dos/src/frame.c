/* SPDX-License-Identifier: MIT */
#include "frame.h"

#define ESCAPE_XOR 0x20

#define STATE_HUNT 0   /* waiting for a delimiter; bytes before it are noise */
#define STATE_BODY 1   /* inside a frame, or between frames with len 0 */
#define STATE_ESCAPE 2 /* the previous byte was 7D */
#define STATE_SKIP 3   /* the frame is invalid; skip to the next delimiter */

/*
 * CRC-16/CCITT-FALSE, a nibble at a time: a 32-byte table instead of
 * 512, which is fast enough for 9600 baud on an 8088.
 */
static const uint16_t crc_nibble[16] = {
    0x0000, 0x1021, 0x2042, 0x3063, 0x4084, 0x50A5, 0x60C6, 0x70E7,
    0x8108, 0x9129, 0xA14A, 0xB16B, 0xC18C, 0xD1AD, 0xE1CE, 0xF1EF
};

uint16_t crc16_update(uint16_t crc, uint8_t byte)
{
    crc = (uint16_t)((crc << 4) ^ crc_nibble[(crc >> 12) ^ (byte >> 4)]);
    crc = (uint16_t)((crc << 4) ^ crc_nibble[(crc >> 12) ^ (byte & 0x0F)]);
    return crc;
}

uint16_t crc16(const uint8_t *data, uint16_t len)
{
    uint16_t crc = 0xFFFF;
    while (len--) {
        crc = crc16_update(crc, *data++);
    }
    return crc;
}

void decoder_init(struct decoder *d, uint8_t *buf, uint16_t max_payload)
{
    d->buf = buf;
    d->limit = (uint16_t)(max_payload + FRAME_HEADER_LEN + FRAME_CRC_LEN);
    d->len = 0;
    d->frame_len = 0;
    d->crc = 0;
    d->state = STATE_HUNT;
    d->reason = FRAME_NONE;
}

static void start(struct decoder *d)
{
    d->len = 0;
    d->state = STATE_BODY;
}

static void skip(struct decoder *d, uint8_t reason)
{
    d->len = 0;
    d->state = STATE_SKIP;
    d->reason = reason;
}

static void store(struct decoder *d, uint8_t byte)
{
    if (d->len == d->limit) {
        skip(d, FRAME_TOO_LONG);
    } else {
        d->buf[d->len++] = byte;
    }
}

static int finish(struct decoder *d)
{
    uint16_t len = d->len;
    uint16_t body;
    uint16_t crc;

    start(d);
    if (len == 0) {
        return FRAME_NONE;
    }
    if (len < FRAME_HEADER_LEN + FRAME_CRC_LEN) {
        return FRAME_TOO_SHORT;
    }
    body = (uint16_t)(len - FRAME_CRC_LEN);
    crc = (uint16_t)(d->buf[body] | (d->buf[body + 1] << 8));
    if (crc16(d->buf, body) != crc) {
        return FRAME_BAD_CRC;
    }
    /* The buffer is not overwritten until the next byte arrives. */
    d->frame_len = body;
    d->crc = crc;
    return FRAME_OK;
}

int decoder_push(struct decoder *d, uint8_t byte)
{
    switch (d->state) {
    case STATE_HUNT:
        if (byte == FRAME_FLAG) {
            start(d);
        }
        return FRAME_NONE;

    case STATE_BODY:
        if (byte == FRAME_FLAG) {
            return finish(d);
        }
        if (byte == FRAME_ESCAPE) {
            d->state = STATE_ESCAPE;
        } else {
            store(d, byte);
        }
        return FRAME_NONE;

    case STATE_ESCAPE:
        if (byte == FRAME_FLAG) {
            /* The 7E also opens the next frame. */
            start(d);
            return FRAME_ABORTED;
        }
        if (byte == 0x5E || byte == 0x5D) {
            d->state = STATE_BODY;
            store(d, (uint8_t)(byte ^ ESCAPE_XOR));
        } else {
            skip(d, FRAME_BAD_ESCAPE);
        }
        return FRAME_NONE;

    default: /* STATE_SKIP */
        if (byte == FRAME_FLAG) {
            start(d);
            return d->reason;
        }
        return FRAME_NONE;
    }
}

int decoder_timeout(struct decoder *d)
{
    int reason;

    switch (d->state) {
    case STATE_HUNT:
        return FRAME_NONE;
    case STATE_BODY:
        if (d->len == 0) {
            return FRAME_NONE;
        }
        reason = FRAME_INCOMPLETE;
        break;
    case STATE_ESCAPE:
        reason = FRAME_INCOMPLETE;
        break;
    default:
        reason = d->reason;
        break;
    }
    d->len = 0;
    d->state = STATE_HUNT;
    return reason;
}

int decoder_in_frame(const struct decoder *d)
{
    return d->state == STATE_ESCAPE || d->state == STATE_SKIP
        || (d->state == STATE_BODY && d->len != 0);
}

static void put_escaped(uint8_t byte, frame_put put)
{
    if (byte == FRAME_FLAG || byte == FRAME_ESCAPE) {
        put(FRAME_ESCAPE);
        put((uint8_t)(byte ^ ESCAPE_XOR));
    } else {
        put(byte);
    }
}

void frame_send(const uint8_t *body, uint16_t len, frame_put put)
{
    uint16_t crc = 0xFFFF;

    put(FRAME_FLAG);
    while (len--) {
        crc = crc16_update(crc, *body);
        put_escaped(*body++, put);
    }
    put_escaped((uint8_t)(crc & 0xFF), put);
    put_escaped((uint8_t)(crc >> 8), put);
    put(FRAME_FLAG);
}
