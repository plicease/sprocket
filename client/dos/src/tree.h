/* SPDX-License-Identifier: MIT */
/*
 * The server's file tree (docs/protocol-v1.1.md): STAT, DIR_LIST and
 * FILE_READ under a root directory, and FILE_BEGIN, FILE_DATA,
 * FILE_COMMIT and FILE_ABORT when writing is allowed. Paths are found
 * ignoring case and never leave the root.
 */
#ifndef SPROCKET_TREE_H
#define SPROCKET_TREE_H

#include <stdint.h>

/* Serve the directory root (a native path); writable allows put. */
void tree_init(const char *root, int writable);

/*
 * Execute a request of type kind with payload in[0 .. in_len). Writes the
 * response payload to out, at most limit bytes, and returns its length;
 * or returns -1 with *class set to an ERROR class and *code to the
 * platform's error code. Types it does not handle get ERR_UNSUPPORTED.
 */
int tree_request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint8_t *out,
                 uint16_t limit, uint8_t *class, uint16_t *code);

/* Abandon a file being written, deleting its temporary file. */
void tree_reset(void);

#endif
