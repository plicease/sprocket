/* SPDX-License-Identifier: MIT */
/*
 * Native protocol v1, the requester's side (docs/protocol-v1.md sections
 * 2 and 3): starting a session with HELLO, then one request at a time,
 * retransmitted until its response arrives. Over the serial port in
 * port.h.
 */
#ifndef SPROCKET_REQ_H
#define SPROCKET_REQ_H

#include <stdint.h>

/* Results of req_hello and req_call. */
#define REQ_OK 0
#define REQ_REFUSED 1   /* an ERROR response: see req_class, req_code, req_message */
#define REQ_LOST 2      /* no response after every retransmission */
#define REQ_CANCELLED 3 /* the user pressed Esc or Ctrl-C while waiting */
#define REQ_BAD 4       /* a response that does not parse */

/* What HELLO_REPLY said. */
struct req_server {
    uint8_t minor; /* the lower of the server's minor version and ours */
    uint16_t max_payload;
    uint32_t capabilities;
    uint8_t platform;
    char version[33];
};

/* The last ERROR response. */
extern uint8_t req_class;
extern uint16_t req_code;
extern char req_message[65];

/*
 * tx must hold max_payload + 4 bytes and rx max_payload + 6. cancel is
 * polled while waiting, and says whether to give up.
 */
void req_init(uint8_t *tx, uint8_t *rx, uint16_t max_payload, long baud, int (*cancel)(void));

/* Where to build a request's payload. */
uint8_t *req_payload(void);

/* The largest payload either side may send, after HELLO. */
uint16_t req_limit(void);

/* Drain stale input, then repeat HELLO with a new session ID about once
 * a second until the server answers or wait seconds pass. */
int req_hello(uint16_t wait, struct req_server *server);

/*
 * Send a request with len bytes of payload from req_payload() and wait
 * for its response. On REQ_OK, *reply and *reply_len give the response
 * payload, valid until the next call.
 */
int req_call(uint8_t kind, uint16_t len, const uint8_t **reply, uint16_t *reply_len);

#endif
