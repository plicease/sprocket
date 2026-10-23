/* SPDX-License-Identifier: MIT */
/*
 * Native protocol v1, client side: sessions, duplicate requests, and the
 * Milestone 2 requests HELLO, PING and CLOSE (docs/protocol-v1.md
 * sections 2 to 4). No I/O: the caller feeds it frames and sends what it
 * returns.
 */
#ifndef SPROCKET_PROTO_H
#define SPROCKET_PROTO_H

#include <stdint.h>

#define MSG_HELLO 0x01
#define MSG_PING 0x02
#define MSG_CLOSE 0x03
#define MSG_RESPONSE 0x80
#define MSG_PENDING 0xFE
#define MSG_ERROR 0xFF

#define ERR_UNSUPPORTED 1
#define ERR_INVALID_REQUEST 2
#define ERR_PROTOCOL_MISMATCH 3

#define PROTO_MAJOR 1
#define PROTO_MINOR 0

#define PLATFORM_DOS 1

struct client {
    /* Set by the caller before client_init. */
    uint16_t max_payload;
    uint32_t capabilities;
    uint8_t platform;
    const char *version;

    /* Session state. */
    uint16_t session; /* 0: none */
    uint16_t host_max_payload;

    /*
     * The last request accepted (seq, type, length, CRC). Its response is
     * kept in resp, which is the duplicate cache.
     */
    uint8_t have_record;
    uint8_t rec_seq;
    uint8_t rec_kind;
    uint16_t rec_len;
    uint16_t rec_crc;
    uint8_t *resp;
    uint16_t resp_len;

    /* Flags for the caller, cleared by it. */
    uint8_t started; /* a HELLO started a new session */
    uint8_t closed;  /* CLOSE was answered */
    uint16_t duplicates;
};

/* resp must hold max_payload + 4 bytes. */
void client_init(struct client *c, uint8_t *resp);

/*
 * Handle one valid frame: body (header and payload) of len bytes, with the
 * frame's CRC. Returns 1 and sets *out and *out_len to the response body
 * to send, or returns 0 if the frame is to be dropped.
 */
int client_handle(struct client *c, const uint8_t *body, uint16_t len,
                  uint16_t crc, const uint8_t **out, uint16_t *out_len);

#endif
