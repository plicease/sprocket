/* SPDX-License-Identifier: MIT */
/*
 * Gathered writes for a file being received (a download, or an upload to
 * the server), so that DOS sees whole, sector-aligned blocks.
 *
 * A DATA payload is the 1 KB limit less its header, 1016 bytes for a
 * download, so writing each as it arrives starts and ends almost every
 * write inside a sector, and DOS must merge those sectors through its
 * buffers. On a NuXT at 9.54 MHz that cost about 24 ms a request, 8% of a
 * download's time at 57600 baud (docs/testing.md, "libc16 build"). The
 * file is written from offset 0 in blocks of WBUF_SIZE, a multiple of the
 * sector size, so every write but the last is whole and aligned.
 *
 * One file at a time: SPROCKET receives one file at a time in either role.
 */
#ifndef SPROCKET_WBUF_H
#define SPROCKET_WBUF_H

#include <stdint.h>
#include <stdio.h>

#define WBUF_SIZE 4096

/* Start gathering writes to f, newly opened for writing. */
void wbuf_begin(FILE *f);

/* Add n bytes. Returns 1, or 0 if writing a block to the file failed
 * (fs_error_code() then has the DOS error). */
int wbuf_write(const void *data, uint16_t n);

/* Write what is pending, before closing the file. Returns 1, or 0 if the
 * write failed. */
int wbuf_end(void);

/* Discard what is pending, before closing a file that is to be removed. */
void wbuf_drop(void);

#endif
