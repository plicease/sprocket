/* SPDX-License-Identifier: MIT */
/*
 * Native protocol v1, client (responder) side: sessions, duplicate
 * requests, and HELLO, PING and CLOSE (docs/protocol-v1.md sections 2 to
 * 4); other requests go to an optional handler, such as the file tree
 * (tree.h). No I/O: the caller feeds it frames and sends what it returns.
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
#define ERR_MEDIA_ERROR 4
#define ERR_FILE_ERROR 5
#define ERR_VERIFY_FAILED 6
/* Minor version 1 (docs/protocol-v1.1.md section 5). */
#define ERR_NOT_FOUND 7
#define ERR_NOT_A_DIRECTORY 8
#define ERR_IS_A_DIRECTORY 9
#define ERR_EXISTS 10
#define ERR_READ_ONLY 11
#define ERR_AMBIGUOUS 12
#define ERR_BAD_NAME 13

/* File messages (protocol v1 section 5 and v1.1 section 6). */
#define MSG_FILE_BEGIN 0x10
#define MSG_FILE_DATA 0x11
#define MSG_FILE_COMMIT 0x12
#define MSG_FILE_ABORT 0x13
#define MSG_STAT 0x18
#define MSG_DIR_LIST 0x19
#define MSG_FILE_READ 0x1A

#define CAP_FILE_WRITE 0x01UL
#define CAP_FILE_REPLACE 0x02UL
#define CAP_FILE_TREE 0x20UL

#define PROTO_MAJOR 1
/* The highest minor version implemented: 1 adds file trees. */
#define PROTO_MINOR 1

#define PLATFORM_DOS 1
#define PLATFORM_LINUX 3

/*
 * Handles requests beyond HELLO, PING and CLOSE, such as tree_request:
 * writes the response payload to out (at most limit bytes) and returns its
 * length, or returns -1 with an ERROR class and platform code.
 */
typedef int (*client_handler)(uint8_t kind, const uint8_t *in, uint16_t in_len,
                              uint8_t *out, uint16_t limit, uint8_t *class,
                              uint16_t *code);

struct client {
    /* Set by the caller before client_init. */
    uint16_t max_payload;
    uint32_t capabilities;
    uint8_t platform;
    const char *version;
    /* Minor version to offer: 0, or 1 with a tree handler. */
    uint8_t minor;
    /* Optional: other requests, and what to undo when a session ends. */
    client_handler handler;
    void (*reset)(void);

    /* Session state. */
    uint16_t session; /* 0: none */
    uint16_t host_max_payload;
    uint8_t session_minor; /* the lower of the host's minor and ours */

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

/* resp must hold max_payload + 4 bytes. Sets minor, handler and reset to
 * their defaults (0, none); set them after this call. */
void client_init(struct client *c, uint8_t *resp);

/*
 * Handle one valid frame: body (header and payload) of len bytes, with the
 * frame's CRC. Returns 1 and sets *out and *out_len to the response body
 * to send, or returns 0 if the frame is to be dropped.
 */
int client_handle(struct client *c, const uint8_t *body, uint16_t len,
                  uint16_t crc, const uint8_t **out, uint16_t *out_len);

#endif
