/* SPDX-License-Identifier: MIT */
/*
 * The server's disks (docs/protocol-v1.md section 6): STORAGE_LIST, and
 * STORAGE_READ by cylinder, head and sector. Only offered when the user
 * asks (SPROCKET SERVER /D).
 */
#ifndef SPROCKET_STORAGE_H
#define SPROCKET_STORAGE_H

#include <stdint.h>

#define MSG_STORAGE_LIST 0x20
#define MSG_STORAGE_READ 0x21

#define CAP_STORAGE_LIST 0x04UL
#define CAP_STORAGE_READ_UNITS 0x08UL
#define CAP_STORAGE_READ_CHS 0x10UL

/* What the disks need in HELLO_REPLY's capabilities. */
#define STORAGE_CAPS (CAP_STORAGE_LIST | CAP_STORAGE_READ_CHS)

/* Find the disks to offer. Returns how many there are. */
uint8_t storage_init(void);

/*
 * Execute a request of type kind, as tree_request does. Types it does not
 * handle get ERR_UNSUPPORTED.
 */
int storage_request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint8_t *out,
                    uint16_t limit, uint8_t *class, uint16_t *code);

#endif
