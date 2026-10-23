/* SPDX-License-Identifier: MIT */
/*
 * Native protocol v1 framing: HDLC-style byte stuffing and CRC-16
 * (docs/protocol-v1.md section 1).
 *
 *   7E | escaped( type | session_lo | session_hi | seq | payload | crc_lo | crc_hi ) | 7E
 *
 * Portable C: built for DOS with Open Watcom and for the host with gcc,
 * so the same code is tested against tests/vectors/.
 */
#ifndef SPROCKET_FRAME_H
#define SPROCKET_FRAME_H

#include <stdint.h>

#define FRAME_FLAG 0x7E
#define FRAME_ESCAPE 0x7D
#define FRAME_HEADER_LEN 4
#define FRAME_CRC_LEN 2
#define FRAME_MIN_PAYLOAD 128
#define FRAME_MAX_PAYLOAD 4090

/* Results of decoder_push and decoder_timeout. */
#define FRAME_NONE 0       /* nothing yet */
#define FRAME_OK 1         /* a valid frame is in the buffer */
#define FRAME_TOO_SHORT 2  /* fewer bytes than a header and CRC */
#define FRAME_BAD_CRC 3
#define FRAME_BAD_ESCAPE 4 /* 7D followed by something other than 5E or 5D */
#define FRAME_ABORTED 5    /* 7D 7E */
#define FRAME_TOO_LONG 6   /* payload over the decoder's maximum */
#define FRAME_INCOMPLETE 7 /* abandoned after the inter-byte timeout */

struct decoder {
    uint8_t *buf;       /* unescaped body and CRC */
    uint16_t limit;     /* buffer size: header, maximum payload and CRC */
    uint16_t len;       /* bytes in buf so far */
    uint16_t frame_len; /* after FRAME_OK, the body length */
    uint16_t crc;       /* after FRAME_OK, the frame's CRC */
    uint8_t state;
    uint8_t reason;     /* why the current frame is being skipped */
};

uint16_t crc16_update(uint16_t crc, uint8_t byte);
uint16_t crc16(const uint8_t *data, uint16_t len);

/* buf must hold max_payload + FRAME_HEADER_LEN + FRAME_CRC_LEN bytes. */
void decoder_init(struct decoder *d, uint8_t *buf, uint16_t max_payload);

/*
 * Feed one received byte. Returns FRAME_OK when a valid frame is complete:
 * its body (header and payload) is d->buf[0 .. d->frame_len) and its CRC
 * is d->crc, valid until the next call. Returns a FRAME_ reason when a frame
 * is dropped, and FRAME_NONE otherwise.
 */
int decoder_push(struct decoder *d, uint8_t byte);

/*
 * Abandon a partial frame after the inter-byte timeout; then ignore bytes
 * until the next delimiter. Returns FRAME_NONE if there was none.
 */
int decoder_timeout(struct decoder *d);

/* Nonzero while part of a frame has been received. */
int decoder_in_frame(const struct decoder *d);

/* Encode a body (header and payload) with its CRC, one byte at a time. */
typedef void (*frame_put)(uint8_t byte);
void frame_send(const uint8_t *body, uint16_t len, frame_put put);

#endif
